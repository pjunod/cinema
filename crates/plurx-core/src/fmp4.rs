//! Fragmented-MP4 surgery for the copy path: read ffmpeg's continuous
//! fragment stream, classify each fragment's opening keyframe, and merge runs
//! of fragments back into single HLS segments.
//!
//! Why this exists. On a `-c:v copy` of an open-GOP source — every disc remux
//! — ffmpeg's HLS muxer cuts a segment at whatever keyframe it finds past the
//! duration floor. Most of those keyframes are CRAs carrying a RASL leading
//! picture, and both Chrome's MSE and Safari's native player treat a segment's
//! first keyframe as a random-access point: the HEVC spec's instruction at
//! random access is to DISCARD the leading pictures, so exactly one frame dies
//! per boundary (`docs/streaming/STUTTER-4K.md` §5.3ter — 11/12 boundary-attributed in
//! Safari, 15/23 in Chrome, and zero drops in 1781 frames of the same
//! bitstream played unsegmented). A copy cannot change GOP structure. It can
//! change **where the cuts land**, and that is all this module does: ffmpeg
//! emits one fragment per GOP into a pipe, and plurx publishes a boundary only
//! in front of a fragment whose first frame is a true random-access point.
//!
//! Everything here is pure — bytes in, bytes out, no I/O and no ffmpeg. The
//! session that drives it lives in `plurxd::copyseg`.
//!
//! Three pieces:
//!
//! - [`FragmentReader`] splits a byte feed into [`Init`] (ftyp+moov, which
//!   normally becomes `init.mp4` verbatim), a [`Fragment`] per GOP, and the trailing
//!   `mfra` index that must never be published.
//! - [`classify`] answers the only question the cut policy asks of a
//!   fragment: is its first frame safe to open a segment with?
//! - [`merge`] concatenates a run of fragments back into one `styp moof mdat`
//!   segment. It moves bytes and rewrites offsets; encoded pictures stay
//!   untouched. The sole bitstream normalization removes VPS/SPS/PPS from
//!   `hvc1`/`dvh1` samples after those exact units have been promoted to hvcC,
//!   which keeps the decoder configuration identical while honoring the
//!   sample entry's out-of-band-only contract.

use std::fmt::Write as _;
use std::ops::Range;

/// What went wrong reading someone else's bytes.
///
/// Split on purpose: [`Fmp4Error::Malformed`] means the stream is not
/// what it claims to be, [`Fmp4Error::Unsupported`] means it is legal MP4 that
/// this reader deliberately does not handle (an explicit `base_data_offset`,
/// say — ffmpeg only writes one without `default_base_moof`, which plurx
/// always passes). The distinction is policy-bearing: malformed input is a
/// reader failure, while Unsupported remains a typed shape fact that the
/// rolling actor may route to one frozen legacy-muxer retry only before media
/// publication. After publication the actor retains the admitted timeline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Fmp4Error {
    /// The bytes do not parse as the box structure they claim.
    #[error("malformed fMP4: {0}")]
    Malformed(String),
    /// Legal MP4 this reader does not implement.
    #[error("unsupported fMP4: {0}")]
    Unsupported(String),
    /// A legal HEVC track has more than one sample description. Mutating
    /// operations cannot choose one, but callers may independently validate
    /// every description before selecting a non-mutating fallback.
    #[error(
        "unsupported fMP4: this stsd has {count} HEVC sample entries; plurx describes one video configuration per track and cannot say which one it means"
    )]
    MultipleHevcSampleEntries { count: usize },
}

/// Output facts read structurally from the one actual AVC sample entry.
/// These are container facts; decoded joins still need separate qualification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcSampleEntryFacts {
    pub codec: String,
    pub width: u16,
    pub height: u16,
    pub color_primaries: u16,
    pub color_transfer: u16,
    pub color_matrix: u16,
    pub full_range: bool,
}

pub fn avc_sample_entry_facts(init: &Init) -> Result<Option<AvcSampleEntryFacts>, Fmp4Error> {
    let Some(codec) = avc_rfc6381_codec(init)? else {
        return Ok(None);
    };
    let video = init.video().expect("AVC codec requires video");
    let (count, entries) = locate_avc_sample_entries(&init.bytes, video.id)?;
    if count != 1 || entries.len() != 1 || entries[0].sample_entry != *b"avc1" {
        return Err(Fmp4Error::Unsupported(
            "continuous AVC requires one avc1 sample entry".into(),
        ));
    }
    let entry = &entries[0];
    let header = peek_box(&init.bytes, entry.entry.start)?
        .ok_or_else(|| Fmp4Error::Malformed("missing AVC sample entry".into()))?;
    let body = entry.entry.start + entry.entry.header_len;
    let end = entry.entry.start + header.size;
    let width = u16::from_be_bytes(
        init.bytes[body + 24..body + 26]
            .try_into()
            .expect("validated visual header"),
    );
    let height = u16::from_be_bytes(
        init.bytes[body + 26..body + 28]
            .try_into()
            .expect("validated visual header"),
    );
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return malformed("continuous AVC has an invalid even raster");
    }
    let colors = find_children(&init.bytes, body + 78..end, b"colr")?;
    if colors.len() != 1 {
        return Err(Fmp4Error::Unsupported(
            "continuous AVC needs one explicit color record".into(),
        ));
    }
    let (at, color) = &colors[0];
    let data = &init.bytes[at.start + color.header_len..at.start + color.size];
    if data.len() != 11 || &data[..4] != b"nclx" || data[10] & 0x7f != 0 {
        return Err(Fmp4Error::Unsupported(
            "continuous AVC needs a complete nclx color record".into(),
        ));
    }
    Ok(Some(AvcSampleEntryFacts {
        codec,
        width,
        height,
        color_primaries: u16::from_be_bytes([data[4], data[5]]),
        color_transfer: u16::from_be_bytes([data[6], data[7]]),
        color_matrix: u16::from_be_bytes([data[8], data[9]]),
        full_range: data[10] & 0x80 != 0,
    }))
}

/// Actual configuration of the initial shared soundtrack family. This parser
/// accepts the production AAC-LC/48 kHz/1024-sample shape, not an arbitrary
/// audio entry whose filename happens to end in MP4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AacSampleEntryFacts {
    pub codec: &'static str,
    pub channels: u16,
    pub sample_rate: u32,
    pub samples_per_frame: u16,
}

pub fn aac_lc_sample_entry_facts(init: &Init) -> Result<AacSampleEntryFacts, Fmp4Error> {
    if init.tracks.len() != 1
        || init.tracks[0].kind != TrackKind::Audio
        || init.tracks[0].timescale != 48_000
    {
        return malformed("shared AAC requires one 48 kHz audio track");
    }
    let bytes = &init.bytes;
    let only_child = |range, kind| -> Result<Range<usize>, Fmp4Error> {
        let boxes = find_children(bytes, range, kind)?;
        let [(at, header)] = boxes.as_slice() else {
            return malformed("shared AAC has a missing or ambiguous configuration box");
        };
        Ok(at.start + header.header_len..at.start + header.size)
    };
    let moov = only_child(0..bytes.len(), b"moov")?;
    let selected = find_children(bytes, moov, b"trak")?
        .into_iter()
        .map(|(at, header)| at.start + header.header_len..at.start + header.size)
        .filter_map(|range| match track_id_in_trak(bytes, range.clone()) {
            Ok(Some(id)) if id == init.tracks[0].id => Some(Ok(range)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let [trak] = selected.as_slice() else {
        return malformed("shared AAC track id is missing or duplicated");
    };
    let mdia = only_child(trak.clone(), b"mdia")?;
    let minf = only_child(mdia, b"minf")?;
    let stbl = only_child(minf, b"stbl")?;
    let stsd = only_child(stbl, b"stsd")?;
    if stsd.len() < 8 || be_u32(bytes, stsd.start + 4) != 1 {
        return malformed("shared AAC requires one sample description");
    }
    let entries = &bytes[stsd.start + 8..stsd.end];
    let descriptions = children(entries)?;
    let [(header, start, end)] = descriptions.as_slice() else {
        return malformed("shared AAC has ambiguous sample descriptions");
    };
    if header.kind() != b"mp4a" || end - start < 28 {
        return malformed("shared AAC has no complete mp4a entry");
    }
    let body = &entries[*start..*end];
    if body[8..10] != [0, 0] || be_u32(body, 24) != 48_000 << 16 {
        return malformed("shared AAC requires a version-zero 48 kHz sample entry");
    }
    let body_start = stsd.start + 8 + start;
    let esds = only_child(body_start + 28..body_start + body.len(), b"esds")?;
    let record = &bytes[esds];
    if record.len() < 4 || record[..4] != [0, 0, 0, 0] {
        return malformed("shared AAC has an unsupported esds version");
    }
    let mut root = &record[4..];
    let (tag, es) = read_audio_descriptor(&mut root)?;
    if tag != 3 || !root.is_empty() || es.len() < 3 || es[2] & 0xe0 != 0 {
        return malformed("shared AAC has an unsupported ES descriptor");
    }
    let mut configs = &es[3..];
    let (tag, decoder) = read_audio_descriptor(&mut configs)?;
    if tag != 4 || decoder.len() < 13 || decoder[0] != 0x40 || decoder[1] != 0x15 {
        return malformed("shared AAC has no MPEG-4 audio decoder configuration");
    }
    let (tag, sl) = read_audio_descriptor(&mut configs)?;
    if tag != 6 || sl != [2] || !configs.is_empty() {
        return malformed("shared AAC has an unsupported synchronization descriptor");
    }
    let mut specific = &decoder[13..];
    let (tag, asc) = read_audio_descriptor(&mut specific)?;
    if tag != 5 || !specific.is_empty() {
        return malformed("shared AAC has ambiguous AudioSpecificConfig");
    }
    parse_shared_aac_config(asc)
}

/// Descriptor lengths are bounded to the MPEG-4 four-byte vocabulary and
/// always checked against their enclosing descriptor before any slice.
fn read_audio_descriptor<'a>(input: &mut &'a [u8]) -> Result<(u8, &'a [u8]), Fmp4Error> {
    let Some((&tag, mut remaining)) = input.split_first() else {
        return malformed("missing MPEG-4 audio descriptor");
    };
    let mut length = 0_usize;
    for _ in 0..4 {
        let Some((&value, rest)) = remaining.split_first() else {
            return malformed("truncated MPEG-4 audio descriptor length");
        };
        remaining = rest;
        length = (length << 7) | usize::from(value & 0x7f);
        if value & 0x80 == 0 {
            if length > remaining.len() {
                return malformed("MPEG-4 audio descriptor exceeds its parent");
            }
            let (body, rest) = remaining.split_at(length);
            *input = rest;
            return Ok((tag, body));
        }
    }
    malformed("MPEG-4 audio descriptor length exceeds four bytes")
}

fn parse_shared_aac_config(config: &[u8]) -> Result<AacSampleEntryFacts, Fmp4Error> {
    if config.len() < 2 {
        return malformed("truncated AAC AudioSpecificConfig");
    }
    let bits = u16::from_be_bytes([config[0], config[1]]);
    let channel_config = (bits >> 3) & 15;
    // Accept no extension, or the encoder's explicit absent-SBR extension.
    // HE-AAC, PCE, short frames and unknown extensions need another family.
    if bits >> 11 != 2
        || (bits >> 7) & 15 != 3
        || bits & 7 != 0
        || !(1..=7).contains(&channel_config)
        || (!config[2..].is_empty() && config[2..] != [0x56, 0xe5, 0x00])
    {
        return malformed("shared AAC requires AAC-LC, 48 kHz and 1024-sample frames");
    }
    Ok(AacSampleEntryFacts {
        codec: "mp4a.40.2",
        channels: if channel_config == 7 {
            8
        } else {
            channel_config
        },
        sample_rate: 48_000,
        samples_per_frame: 1_024,
    })
}

/// The validated HEVC sample-description shape carried by an initialization
/// segment. Every reported description has already had its decoder
/// configuration checked; this is therefore safe policy input rather than an
/// inference from a parser error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HevcSampleEntryLayout {
    NotHevc,
    Single,
    Multiple { count: usize },
}

/// Derive the exact RFC 6381 AVC identity from the selected video track.
///
/// The decoder configuration is accepted only when every sample description
/// on that track is AVC and every description names the same sample-entry
/// fourcc and profile/compatibility/level triplet. This is deliberately
/// stricter than finding an `avcC` byte sequence: payloads in unrelated boxes
/// and ambiguous alternate descriptions must not become playlist identity.
pub fn avc_rfc6381_codec(init: &Init) -> Result<Option<String>, Fmp4Error> {
    let Some(video) = init.video() else {
        return Ok(None);
    };
    if video.codec != Some(VideoCodec::H264) {
        return Ok(None);
    }
    let (description_count, locations) = locate_avc_sample_entries(&init.bytes, video.id)?;
    if locations.is_empty() || locations.len() != description_count {
        return Err(Fmp4Error::Unsupported(
            "the H.264 video track has a missing or non-AVC sample description".into(),
        ));
    }

    let mut codec = None;
    for location in locations {
        let record = &init.bytes[location.payload];
        let [1, profile, compatibility, level, ..] = record else {
            return malformed("avcC has no complete profile/compatibility/level header");
        };
        if *profile == 0 || *level == 0 {
            return malformed("avcC carries an unassigned profile or level");
        }
        let current = format!(
            "{}.{profile:02X}{compatibility:02X}{level:02X}",
            fourcc(&location.sample_entry)
        );
        match &codec {
            None => codec = Some(current),
            Some(first) if first == &current => {}
            Some(_) => {
                return Err(Fmp4Error::Unsupported(
                    "the H.264 video track has ambiguous sample descriptions".into(),
                ));
            }
        }
    }
    Ok(codec)
}

fn malformed<T>(msg: impl Into<String>) -> Result<T, Fmp4Error> {
    Err(Fmp4Error::Malformed(msg.into()))
}

/// Largest box this reader will believe in.
///
/// Two jobs. It is a sanity bound — one fragment is one GOP, and a GOP that
/// serialized to two gigabytes is a stream nobody produced — and it is what
/// makes every `pos + size` in this module safe to write as a plain add: with
/// `size` bounded here and `pos` bounded by a buffer that had to be allocated,
/// the sum cannot wrap. Without it, a 64-bit `largesize` of `2^64 - 16` makes
/// `pos + size` wrap to *behind* the cursor: the reader spins forever on the
/// same bytes in release, and hands the merger a fragment whose mdat range
/// runs backwards. Both were found by review, both are one hostile box away.
///
/// `i32::MAX` rather than a round number, because a merged segment's `trun`
/// data offsets are signed 32-bit and the merger refuses anything larger
/// anyway — a box this reader accepted but the merger could not address would
/// be a failure deferred rather than avoided.
const MAX_BOX_BYTES: usize = i32::MAX as usize;

/// Largest `sample_count` a single `trun` may declare.
///
/// The length check below it is necessary but NOT sufficient: a `trun` that
/// sets none of the per-sample flags (legal — every value then comes from the
/// `tfhd`/`trex` defaults) has a per-sample cost of zero bytes, so an
/// eight-byte box can declare four billion samples and pass. `Vec` then tries
/// to reserve 64 GB, and a failed allocation is an abort, not an error: the
/// whole daemon dies rather than the one session falling back to ffmpeg's
/// muxer. Found by review, with a 76-byte `moof` that reproduces it.
///
/// A million samples is ~11 hours of 24 fps video or ~6 hours of AAC in ONE
/// fragment, against fragments that are one GOP each. Nothing real comes near
/// it, and the allocation it bounds is 16 MB.
const MAX_SAMPLES_PER_RUN: usize = 1 << 20;

/// What a track carries. Only `Video` is load-bearing — it is the track whose
/// keyframes decide where segments may be cut and whose durations become
/// `EXTINF` — but audio has to be identified to be carried along faithfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Other,
}

/// The two codecs a plurx copy session can produce, and the only two whose
/// keyframes this module knows how to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    Hevc,
    H264,
}

/// One track, as the `moov` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: u32,
    pub kind: TrackKind,
    /// Ticks per second for this track's durations. Video's is the one
    /// `EXTINF` is computed in — audio's differs by up to one AAC frame per
    /// fragment and using it drifts the playlist against the media.
    pub timescale: u32,
    /// Structural edit metadata; encoded VOD must never compensate its grid through it.
    pub has_edit_list: bool,
    pub codec: Option<VideoCodec>,
    /// The sample entry carries a Dolby Vision decoder configuration (`dvcC`
    /// or `dvvC`). This is distinct from an `hvc1` base layer: ffmpeg may strip
    /// the Dolby configuration and RPUs while leaving the file-level `dby1`
    /// compatible brand behind.
    pub dolby_vision_config: bool,
    /// Bytes of the length prefix on each NAL unit inside a sample, from
    /// `hvcC`/`avcC`. Zero when the track has no such record.
    pub nal_length_size: u8,
    /// `trex` defaults — the last resort when neither `trun` nor `tfhd`
    /// carries a value. ffmpeg always fills `tfhd`, so these are insurance.
    pub default_sample_duration: u32,
    pub default_sample_size: u32,
    pub default_sample_flags: u32,
}

/// `ftyp` + `moov`: the initialization segment, normally kept verbatim.
///
/// The intentional exceptions are [`promote_hevc_parameter_sets`] and
/// [`promote_hdr10_static_metadata`]. ffmpeg can leave decoder-wide HEVC
/// records only in the first media sample. Apple HLS requires them in the
/// configuration carried by this segment, so the copy session may add those
/// exact NAL units to `hvcC` before publishing it. [`merge`] then removes the
/// promoted VPS/SPS/PPS from `hvc1`/`dvh1` media samples so the published
/// stream keeps the sample entry's out-of-band-only promise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Init {
    pub bytes: Vec<u8>,
    pub tracks: Vec<Track>,
}

impl Init {
    /// The video track, or `None` on an audio-only stream (which the copy
    /// path never produces, but a parser should not assume).
    pub fn video(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Video)
    }

    pub fn track(&self, id: u32) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
}

/// One sample, with every value resolved.
///
/// "Resolved" is the point: MP4 lets a value live in the `trun`, or in the
/// `tfhd` defaults, or in the `trex` defaults, or (for sample zero only) in
/// the `trun`'s first-sample-flags. Resolving at parse time means the merger
/// and the classifier never re-implement that ladder, and the merged output
/// can write every value explicitly and be done with defaults entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub duration: u32,
    pub size: u32,
    /// Byte offset of this sample's `sample_size` field within the fragment's
    /// bytes, when the `trun` carries one per sample.
    ///
    /// Recorded so a rewrite that changes a sample's length can correct the
    /// declaration in place. `None` means the run used `default_sample_size`
    /// from the `tfhd`; the rewrite materializes an explicit field before it
    /// changes the sample.
    pub size_at: Option<usize>,
    pub flags: u32,
    /// Composition offset (pts − dts). Signed here even though ffmpeg writes
    /// version-0 unsigned offsets over a shifted dts, because a version-1
    /// `trun` is legal and negative is what it means.
    pub cto: i64,
}

impl Sample {
    /// Whether this sample is a sync sample — bit 16 of `sample_flags` is
    /// `sample_is_non_sync_sample`, so the test is inverted.
    pub fn is_sync(&self) -> bool {
        self.flags & 0x0001_0000 == 0
    }
}

/// One `trun`, resolved: where its sample data starts and what its samples are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Original trun wire shape, retained for encoded presentation-grid validation.
    pub version: u8,
    pub composition_offsets_present: bool,
    /// Byte offset of this run's first sample, relative to the start of the
    /// enclosing [`Fragment`]'s bytes (which begin at the `moof` — the same
    /// origin `default-base-is-moof` gives the `trun`).
    pub data_offset: usize,
    /// Byte offset of the `data_offset` *field* within the fragment's bytes,
    /// when this `trun` carries one.
    ///
    /// A run whose offset was inherited from the previous run has `None`: its
    /// position follows from the samples before it, so correcting those
    /// corrects this. Recorded for the same reason as
    /// [`Sample::size_at`] — a rewrite that changes sample lengths has to move
    /// every later run, and this is where it says so.
    pub data_offset_at: Option<usize>,
    pub samples: Vec<Sample>,
}

impl Run {
    fn byte_len(&self) -> usize {
        self.samples.iter().map(|s| s.size as usize).sum()
    }
}

/// One track's share of one fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackFragment {
    pub track_id: u32,
    /// `tfdt` — the decode time of this fragment's first sample.
    pub base_decode_time: u64,
    pub runs: Vec<Run>,
}

impl TrackFragment {
    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.runs.iter().flat_map(|r| r.samples.iter())
    }

    pub fn sample_count(&self) -> usize {
        self.runs.iter().map(|r| r.samples.len()).sum()
    }

    /// Summed sample durations, in this track's timescale. This — not wall
    /// clock, not the audio track — is what `EXTINF` is made of.
    pub fn duration(&self) -> u64 {
        self.samples().map(|s| s.duration as u64).sum()
    }

    /// Summed sample sizes — this track's payload bytes in this fragment, with
    /// no container overhead.
    ///
    /// The one quantity that survives a change of pipeline: video samples are
    /// copied, so a generation carrying different audio, or none, emits byte
    /// for byte the same ones. `crate::segplan`'s landing matcher is built on
    /// exactly that, which is why the wire length will not do — see
    /// [`crate::segplan::IndexRow::video_bytes`].
    pub fn byte_len(&self) -> usize {
        self.runs.iter().map(Run::byte_len).sum()
    }
}

/// One `moof` + `mdat` pair: with `frag_keyframe`, exactly one GOP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// The fragment verbatim, `moof` through end of `mdat`.
    pub bytes: Vec<u8>,
    /// The `mdat` payload's range within [`Fragment::bytes`] — i.e. past the
    /// box header. Every `Run::data_offset` points inside it.
    pub mdat_payload: Range<usize>,
    pub tracks: Vec<TrackFragment>,
}

impl Fragment {
    pub fn track(&self, id: u32) -> Option<&TrackFragment> {
        self.tracks.iter().find(|t| t.track_id == id)
    }

    /// Bytes on the wire, which is what the segment byte ceiling counts.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Video duration in the video track's timescale, or 0 if this fragment
    /// carries no video.
    pub fn video_duration(&self, init: &Init) -> u64 {
        init.video()
            .and_then(|v| self.track(v.id))
            .map(|t| t.duration())
            .unwrap_or(0)
    }
}

/// What [`FragmentReader::next_unit`] hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Init(Init),
    Fragment(Fragment),
    /// The `mfra` random-access index ffmpeg appends at EOF. Recognised so it
    /// is never mistaken for payload; it is never published.
    Trailer,
}

/// Splits ffmpeg's `-f mp4 … pipe:` output into init, fragments and trailer.
///
/// Streaming and patient. `delay_moov` means the pipe opens with `ftyp` and
/// then goes quiet until the first packets commit, and a SIGSTOPped ffmpeg
/// (the ahead-window suspend) can leave minutes between reads — so this holds
/// whatever it has and answers `Ok(None)` until a whole unit has arrived,
/// forever if need be.
#[derive(Debug, Default)]
pub struct FragmentReader {
    buf: Vec<u8>,
    pos: usize,
    /// Where `ftyp` began, so `init.mp4` is `ftyp`+`moov` and not any padding
    /// that preceded them. Only meaningful before the init is emitted.
    init_start: Option<usize>,
    init_done: bool,
    /// The track table from the `moov`, kept after the [`Init`] is handed out
    /// because the `trex` defaults on it are the bottom rung of every
    /// fragment's sample-resolution ladder. ffmpeg always fills `tfhd` and so
    /// never reaches them, which is exactly why they had to be wired up
    /// deliberately: a rung nothing exercises is a rung that quietly does not
    /// exist, and a stream that did lean on `trex` would have resolved every
    /// sample to zero duration — a segmenter that never reaches its floor and
    /// holds the whole film in memory.
    tracks: Vec<Track>,
    saw_trailer: bool,
    reject_reinitialization: bool,
}

impl FragmentReader {
    pub fn new() -> FragmentReader {
        FragmentReader::default()
    }

    /// A guarded publication keeps one decoder configuration for its lifetime.
    pub fn reject_reinitialization(&mut self) {
        self.reject_reinitialization = true;
    }

    /// Feed bytes read from the pipe.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Bytes held but not yet formed into a unit. The session watches this:
    /// the byte ceiling bounds a segment, so a reader holding far more than
    /// one segment's worth is a bug, not a busy moment.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// The next complete unit, or `Ok(None)` when more bytes are needed.
    ///
    /// `Ok(None)` on a truncated feed is deliberate and permanent: a killed
    /// session's last partial fragment is not an error to report, it is a
    /// fragment that will never finish.
    pub fn next_unit(&mut self) -> Result<Option<Unit>, Fmp4Error> {
        loop {
            let Some(hdr) = peek_box(&self.buf, self.pos)? else {
                return Ok(None);
            };
            if !self.have(&hdr) {
                return Ok(None);
            }

            if !self.init_done {
                match hdr.kind() {
                    b"ftyp" => {
                        if self.init_start.is_none() {
                            self.init_start = Some(self.pos);
                        }
                        self.pos += hdr.size;
                    }
                    b"moov" => {
                        let start = self.init_start.unwrap_or(self.pos);
                        let end = self.pos + hdr.size;
                        let tracks = parse_moov(&self.buf[self.pos + hdr.header_len..end])?;
                        let bytes = self.buf[start..end].to_vec();
                        self.pos = end;
                        self.init_done = true;
                        self.init_start = None;
                        self.tracks = tracks.clone();
                        self.compact();
                        return Ok(Some(Unit::Init(Init { bytes, tracks })));
                    }
                    // `free`/`skip` padding before the real boxes is legal and
                    // must not become part of `init.mp4`.
                    _ => self.pos += hdr.size,
                }
                continue;
            }

            match hdr.kind() {
                b"ftyp" | b"moov" if self.reject_reinitialization => {
                    return malformed("guarded stream replaced its initialization segment");
                }
                b"moof" => {
                    let mdat_pos = self.pos + hdr.size;
                    let Some(mdat) = peek_box(&self.buf, mdat_pos)? else {
                        return Ok(None);
                    };
                    if mdat.kind() != b"mdat" {
                        return malformed(format!(
                            "expected mdat after moof, found {}",
                            fourcc(mdat.kind())
                        ));
                    }
                    if self.buf.len() < mdat_pos + mdat.size {
                        return Ok(None);
                    }
                    let end = mdat_pos + mdat.size;
                    let tracks = parse_moof(
                        &self.buf[self.pos..self.pos + hdr.size],
                        hdr.header_len,
                        &self.tracks,
                    )?;
                    let bytes = self.buf[self.pos..end].to_vec();
                    let payload_start = hdr.size + mdat.header_len;
                    let fragment = Fragment {
                        mdat_payload: payload_start..bytes.len(),
                        bytes,
                        tracks,
                    };
                    self.pos = end;
                    self.compact();
                    return Ok(Some(Unit::Fragment(fragment)));
                }
                b"mfra" => {
                    self.pos += hdr.size;
                    self.saw_trailer = true;
                    self.compact();
                    return Ok(Some(Unit::Trailer));
                }
                // `sidx`, `free`, `skip`, a stray `styp` — none of them are
                // payload and none of them are errors.
                _ => self.pos += hdr.size,
            }
        }
    }

    /// True once the `mfra` index has been seen — i.e. ffmpeg finished
    /// cleanly rather than being killed.
    pub fn saw_trailer(&self) -> bool {
        self.saw_trailer
    }

    fn have(&self, hdr: &BoxHeader) -> bool {
        self.buf.len() >= self.pos + hdr.size
    }

    /// Drop what has been consumed. Only ever called right after a unit is
    /// emitted, so the memmove covers one partial box, never the backlog.
    fn compact(&mut self) {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BoxHeader {
    size: usize,
    header_len: usize,
    ty: [u8; 4],
}

impl BoxHeader {
    fn kind(&self) -> &[u8; 4] {
        &self.ty
    }
}

fn fourcc(ty: &[u8; 4]) -> String {
    ty.iter()
        .map(|b| {
            if b.is_ascii_graphic() {
                *b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Read a box header at `pos`. `Ok(None)` means "not enough bytes yet".
fn peek_box(buf: &[u8], pos: usize) -> Result<Option<BoxHeader>, Fmp4Error> {
    peek_box_before(buf, pos, None)
}

/// Read a box header whose complete parent boundary is already known.
///
/// A nested declaration that exceeds that boundary is malformed even when its
/// size also exceeds this reader's resource ceiling. Top-level streaming input
/// has no such proof yet and remains unsupported when it exceeds that ceiling.
fn peek_box_before(
    buf: &[u8],
    pos: usize,
    parent_end: Option<usize>,
) -> Result<Option<BoxHeader>, Fmp4Error> {
    if buf.len() < pos + 8 {
        return Ok(None);
    }
    let size32 = be_u32(buf, pos) as usize;
    let mut ty = [0u8; 4];
    ty.copy_from_slice(&buf[pos + 4..pos + 8]);
    let (size, header_len) = match size32 {
        1 => {
            if buf.len() < pos + 16 {
                return Ok(None);
            }
            let large = be_u64(buf, pos + 8);
            (large, 16)
        }
        // "to end of file" is legal in a file and meaningless in a stream that
        // has not ended. ffmpeg never writes it for the boxes on this path.
        0 => {
            return Err(Fmp4Error::Unsupported(format!(
                "box {} with size 0 (extends to EOF)",
                fourcc(&ty)
            )))
        }
        n => (n as u64, 8),
    };
    if size < header_len as u64 {
        return malformed(format!("box {} declares size {size}", fourcc(&ty)));
    }
    if let Some(parent_end) = parent_end {
        let Some(parent_remaining) = parent_end.checked_sub(pos) else {
            return malformed("box starts past its parent");
        };
        if size > parent_remaining as u64 {
            return malformed(format!("box {} runs past its parent", fourcc(&ty)));
        }
    }
    if size > MAX_BOX_BYTES as u64 {
        return Err(Fmp4Error::Unsupported(format!(
            "box {} declares {size} bytes",
            fourcc(&ty)
        )));
    }
    let size = size as usize;
    Ok(Some(BoxHeader {
        size,
        header_len,
        ty,
    }))
}

/// A child box: its header and the half-open range of its payload within the
/// parent slice. Ranges are returned as a pair rather than a `Range` so
/// callers can index a slice twice without cloning.
type Child = (BoxHeader, usize, usize);

/// Iterate the child boxes of a payload slice.
fn children(payload: &[u8]) -> Result<Vec<Child>, Fmp4Error> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= payload.len() {
        let Some(hdr) = peek_box_before(payload, p, Some(payload.len()))? else {
            break;
        };
        let Some(end) = p.checked_add(hdr.size) else {
            return malformed("box size overflows");
        };
        if end > payload.len() {
            return malformed(format!("box {} runs past its parent", fourcc(hdr.kind())));
        }
        out.push((hdr, p + hdr.header_len, end));
        p = end;
    }
    Ok(out)
}

fn be_u32(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])
}

fn be_u64(b: &[u8], p: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[p..p + 8]);
    u64::from_be_bytes(a)
}

// ---------------------------------------------------------------------------
// moov
// ---------------------------------------------------------------------------

/// Parse the track table out of a `moov` payload.
fn parse_moov(payload: &[u8]) -> Result<Vec<Track>, Fmp4Error> {
    let mut tracks = Vec::new();
    let mut trex: Vec<(u32, u32, u32, u32)> = Vec::new();
    for (hdr, start, end) in children(payload)? {
        match hdr.kind() {
            b"trak" => tracks.push(parse_trak(&payload[start..end])?),
            b"mvex" => {
                let mvex = &payload[start..end];
                for (h2, s2, e2) in children(mvex)? {
                    if h2.kind() != b"trex" {
                        continue;
                    }
                    let b = &mvex[s2..e2];
                    if b.len() < 24 {
                        return malformed("trex too short");
                    }
                    trex.push((be_u32(b, 4), be_u32(b, 12), be_u32(b, 16), be_u32(b, 20)));
                }
            }
            _ => {}
        }
    }
    if tracks.is_empty() {
        return malformed("moov declares no tracks");
    }
    for t in &mut tracks {
        if let Some((_, dur, size, flags)) = trex.iter().find(|(id, ..)| *id == t.id) {
            t.default_sample_duration = *dur;
            t.default_sample_size = *size;
            t.default_sample_flags = *flags;
        }
    }
    Ok(tracks)
}

fn parse_trak(payload: &[u8]) -> Result<Track, Fmp4Error> {
    let mut track = Track {
        id: 0,
        kind: TrackKind::Other,
        timescale: 0,
        has_edit_list: false,
        codec: None,
        dolby_vision_config: false,
        nal_length_size: 0,
        default_sample_duration: 0,
        default_sample_size: 0,
        default_sample_flags: 0,
    };
    for (hdr, start, end) in children(payload)? {
        let b = &payload[start..end];
        match hdr.kind() {
            b"tkhd" => {
                // version 0: creation(4) modification(4) track_id(4)
                // version 1: creation(8) modification(8) track_id(4)
                let off = if b.first().copied().unwrap_or(0) == 1 {
                    20
                } else {
                    12
                };
                if b.len() < off + 4 {
                    return malformed("tkhd too short");
                }
                track.id = be_u32(b, off);
            }
            b"mdia" => parse_mdia(b, &mut track)?,
            b"edts" | b"elst" => track.has_edit_list = true,
            _ => {}
        }
    }
    if track.id == 0 {
        return malformed("trak has no track_id");
    }
    Ok(track)
}

fn parse_mdia(payload: &[u8], track: &mut Track) -> Result<(), Fmp4Error> {
    for (hdr, start, end) in children(payload)? {
        let b = &payload[start..end];
        match hdr.kind() {
            b"mdhd" => {
                let off = if b.first().copied().unwrap_or(0) == 1 {
                    20
                } else {
                    12
                };
                if b.len() < off + 4 {
                    return malformed("mdhd too short");
                }
                track.timescale = be_u32(b, off);
            }
            // Guard rather than a nested `if`: a short hdlr falls through to
            // the `_ => {}` arm below, which is what the nested form did too.
            b"hdlr" if b.len() >= 12 => {
                track.kind = match &b[8..12] {
                    b"vide" => TrackKind::Video,
                    b"soun" => TrackKind::Audio,
                    _ => TrackKind::Other,
                };
            }
            b"minf" => {
                for (h2, s2, e2) in children(b)? {
                    if h2.kind() == b"stbl" {
                        parse_stbl(&b[s2..e2], track)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_stbl(payload: &[u8], track: &mut Track) -> Result<(), Fmp4Error> {
    for (hdr, start, end) in children(payload)? {
        if hdr.kind() != b"stsd" {
            continue;
        }
        // stsd payload: version/flags(4) entry_count(4) then sample entries.
        let b = &payload[start..end];
        if b.len() < 8 {
            return malformed("stsd too short");
        }
        let entries = &b[8..];
        for (entry, estart, eend) in children(entries)? {
            let codec = match entry.kind() {
                b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => Some(VideoCodec::Hevc),
                b"avc1" | b"avc3" | b"dva1" | b"dvav" => Some(VideoCodec::H264),
                _ => None,
            };
            let Some(codec) = codec else { continue };
            track.codec = Some(codec);
            // VisualSampleEntry: 8 bytes of reserved + data_reference_index,
            // then 70 bytes of fixed video fields before the child boxes. The
            // payload range already skips the 8-byte box header.
            let body = &entries[estart..eend];
            if body.len() < 78 {
                return malformed("visual sample entry too short");
            }
            let extra = &body[78..];
            for (h2, s2, e2) in children(extra)? {
                let rec = &extra[s2..e2];
                match (h2.kind(), codec) {
                    (b"dvcC" | b"dvvC", _) => {
                        track.dolby_vision_config = true;
                    }
                    (b"hvcC", VideoCodec::Hevc) => {
                        // Byte 21 of the HEVCDecoderConfigurationRecord packs
                        // lengthSizeMinusOne into its low two bits.
                        if rec.len() < 22 {
                            return malformed("hvcC too short");
                        }
                        track.nal_length_size = (rec[21] & 3) + 1;
                    }
                    (b"avcC", VideoCodec::H264) => {
                        if rec.len() < 5 {
                            return malformed("avcC too short");
                        }
                        track.nal_length_size = (rec[4] & 3) + 1;
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HEVC initialization metadata
// ---------------------------------------------------------------------------

/// Promote in-band HEVC VPS/SPS/PPS units into the decoder configuration.
///
/// A few WEB-DL Matroska sources carry the 23-byte minimum hvcC record (zero
/// NAL arrays) and put all three parameter sets in their first sample. That is
/// decodable as an in-band `hev1`/`dvhe` stream, but a `hvc1`/`dvh1` HLS sample
/// entry promises the decoder configuration in hvcC. AVPlayer validates that
/// promise before fetching media and rejects the empty record. The copy
/// segmenter already holds the init until this first sample arrives, so copy
/// the authored units into hvcC without changing the encoded picture or Dolby
/// Vision RPU data.
/// Everything the two promotions copy into `hvcC`, captured away from the
/// generation that will use it.
///
/// Both promotions read **the first video sample of the fragment they are
/// handed**, which for the live path is the first fragment the session
/// produced and is exactly right: a session with no plan has nothing else to
/// go on, and the init it writes describes the media that follows it.
///
/// A VOD rendition cannot work that way. Its `init.mp4` is written once and
/// then served for the life of the rendition, while its producer is
/// repositioned all over the film — so an init promoted from whichever
/// fragment a generation happened to land on is a different byte string per
/// generation, and plan §2.2 refuses the second one. The two requirements are
/// opposites: promotion wants the init to describe what follows, and §2.2
/// wants it to be the same regardless of what follows.
///
/// Capturing the inputs once dissolves that. The indexer takes them from the
/// film's opening clean fragment — film-level facts, and the index pipe emits
/// byte-identical video samples to the production pipe, which M0 measured
/// 28/28 — and every later generation promotes from the stored copy. Promotion
/// becomes a pure function of facts that do not depend on where a producer
/// started, so byte-identity holds by construction rather than by luck, and
/// §2.2's check becomes an assertion that cannot fire spuriously.
///
/// The failure this prevents is not only variation. Mastering-display SEI need
/// not appear on every GOP, so a generation landing on a fragment without it
/// promotes *nothing* and produces an init missing boxes the stored one has —
/// a refused generation whose media the stored init describes perfectly.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromotionInputs {
    /// Canonical HDR metadata from a complete index scan. Old persisted
    /// renditions default to fill-only promotion and keep their exact bytes.
    #[serde(default)]
    pub indexed_hdr10_metadata: bool,
    /// Complete original-header scan; old persisted records remain unverified.
    #[serde(default)]
    pub hevc_configuration: Option<crate::hevc_configuration::Proof>,
    /// HEVC VPS/SPS/PPS NAL units (types 32-34), in the order found.
    pub parameter_sets: Vec<Vec<u8>>,
    /// Opening-sample evidence for FFmpeg's dvh1 completeness-bit omission.
    /// Separate from missing-type promotion: later GOPs need not repeat it.
    /// Empty on old persisted indexes, which retain their previous behavior.
    #[serde(default)]
    pub dvh1_parameter_sets: Vec<Vec<u8>>,
    /// Malformed opening evidence must not become an absent later repetition.
    #[serde(default)]
    pub dvh1_parameter_sets_malformed: bool,
    /// Prefix-SEI NAL units carrying HDR10 static metadata.
    pub hdr10_sei: Vec<Vec<u8>>,
    /// The Dolby Vision configuration record to write into the served init.
    ///
    /// Unlike the two above, this does not come out of a fragment — it comes
    /// out of the *source file's* stored facts, because the record the muxer
    /// wrote describes the wrong stream. ffmpeg does not derive this record
    /// from the RPUs: it copies the one its input container had (measured,
    /// `docs/streaming/PLAYBACK-CAPS-V2-M0.md` §8). A Profile 7 → 8.1 conversion runs on
    /// the far side of that muxer, so the output carries the *source's*
    /// Profile 7 `dvcC` over samples whose RPUs now say 8.1 — a sample entry
    /// declaring an enhancement layer the conversion dropped, which is worse
    /// than no record at all. [`set_dolby_vision_record`] replaces it in place
    /// (the payload is the same 24 bytes in both spellings, so only the box
    /// name and its contents change) or appends one when the muxer wrote none.
    ///
    /// It belongs *here*, in the stored promotion inputs, for the reason the
    /// type exists at all: promotion has to be a pure function of facts that
    /// do not depend on where a producer started, or a regenerated init
    /// differs from the stored one and the generation is refused. A record
    /// written anywhere else would be written on one path and not the other.
    #[serde(default)]
    pub dolby_vision: Option<DolbyVisionRecord>,

    /// Delete the muxer's Dolby Vision record instead of replacing it.
    ///
    /// The strip path's counterpart to `dolby_vision`, and it lives here for
    /// the same reason: promotion has to be a pure function of stored facts,
    /// or a regenerated init differs from the stored one and the generation is
    /// refused. A removal performed anywhere else would happen on the live
    /// path and not on a head regeneration.
    ///
    /// Set when the copy strips Dolby Vision on an ffmpeg without `dovi_rpu`.
    /// `filter_units` removes the RPU and enhancement-layer NAL units by type
    /// and knows nothing about the DOVI side data ffmpeg copied out of the
    /// source container, so the muxer writes a Profile 7 record with
    /// `el_present_flag = 1` over a stream that has neither layer. See
    /// [`remove_dolby_vision_record`] for what that costs a viewer.
    ///
    /// Mutually exclusive with `dolby_vision`: one says "make the record
    /// describe the converted stream", the other says "there is no Dolby
    /// Vision stream to describe". [`promote_from`] refuses both at once
    /// rather than picking, because a caller that set both has a bug that a
    /// precedence rule would hide.
    ///
    /// `#[serde(default)]`, so a rendition stored before this field existed
    /// deserializes as `false` and promotes exactly as it did.
    #[serde(default)]
    pub strip_dolby_vision: bool,
}

impl PromotionInputs {
    /// What promotion would copy out of this fragment.
    ///
    /// Answers empty for every source promotion no-ops on, which is most of
    /// them: a normal HEVC encode writes a populated `hvcC` and carries no
    /// parameter sets in band.
    pub fn from_fragment(fragment: &Fragment, init: &Init) -> PromotionInputs {
        let Some(video) = init.video() else {
            return PromotionInputs::default();
        };
        if video.codec != Some(VideoCodec::Hevc) || video.nal_length_size == 0 {
            return PromotionInputs::default();
        }
        let Some(sample) = first_video_sample(fragment, video) else {
            return PromotionInputs::default();
        };
        // A type the sample entry already configures is never promoted —
        // `promote_hevc_parameter_sets_from` skips it — so it is not a
        // promotion input. Counting it would make a film whose redefinitions
        // travel in band (`CopyVideoOptions::retains_hevc_parameter_sets`)
        // look like one whose clean starts disagree about their init, when
        // every one of them promotes to the same init.
        let configured = locate_configuring_hvcc(&init.bytes)
            .ok()
            .flatten()
            .and_then(|location| hvcc_nal_array_types(&init.bytes[location.payload]).ok())
            .unwrap_or_default();
        let dvh1_parameter_sets = dvh1::capture(init, sample, video.nal_length_size);
        PromotionInputs {
            indexed_hdr10_metadata: false,
            hevc_configuration: None,
            // Never from a fragment: a fragment has no record and no facts to
            // build one from. Both answers are the caller's, taken from the
            // source file and the session's own copy options.
            dolby_vision: None,
            strip_dolby_vision: false,
            dvh1_parameter_sets_malformed: dvh1_parameter_sets.is_err(),
            dvh1_parameter_sets: dvh1_parameter_sets.unwrap_or_default(),
            parameter_sets: hevc_parameter_set_nals(sample, video.nal_length_size)
                .into_iter()
                .filter(|nal| hevc_nal_type(nal).is_some_and(|kind| !configured.contains(&kind)))
                .map(<[u8]>::to_vec)
                .collect(),
            hdr10_sei: hdr10_prefix_sei_nals(sample, video.nal_length_size)
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect(),
        }
    }

    /// An absent later tuple is ordinary; a present conflicting tuple is not.
    /// The opening proof is reused by every planned generation, independently
    /// of whether its landing GOP repeats the out-of-band configuration.
    pub fn matches_later_parameter_sets(&self, later: &Self) -> bool {
        let mut later = later.clone();
        if later.dvh1_parameter_sets.is_empty() {
            later
                .dvh1_parameter_sets
                .clone_from(&self.dvh1_parameter_sets);
        }
        self == &later
    }

    pub fn is_empty(&self) -> bool {
        !self.dvh1_parameter_sets_malformed
            && self.dvh1_parameter_sets.is_empty()
            && self.parameter_sets.is_empty()
            && self.hdr10_sei.is_empty()
            && self.dolby_vision.is_none()
            && !self.strip_dolby_vision
    }
}

/// Rewrite every video sample in a fragment, correcting every size the
/// rewrite invalidates.
///
/// `rewrite` is handed one sample's bytes and returns its replacement. It is
/// free to change the length; everything that has to be told about that is
/// this function's job:
///
/// - the sample's own `sample_size` in its `trun`;
/// - every later run's `data_offset`, by the length change accumulated before
///   it;
/// - the `mdat` box size.
///
/// Ordinarily the `moof` never changes size, which makes this a patch rather
/// than a rebuild. A legal `trun` may instead take every sample size from its
/// `tfhd`; in that case this first materializes one size field per sample,
/// grows the containing boxes, and moves every declared media-data offset.
///
/// **Refuses rather than guessing.** A later run without its own `data_offset`
/// cannot express a per-sample rewrite: its position is implied by bytes this
/// function is changing. So is an `mdat` whose size cannot be corrected in
/// place. Audio and any other track's samples are untouched and their runs
/// still move, which is why their offsets are corrected too.
///
/// **A refusal leaves the fragment exactly as it was**, at every step, not
/// only while the edits are being collected: every fallible decision is made
/// before a byte moves, and the result is built into a fresh buffer that
/// replaces the fragment's only once the re-parse has proved it readable.
/// This is the invariant the callers' safety is argued from — `plurxd`'s
/// session fails the generation on a refusal and its index pass returns
/// `Unsupported`, and neither expects the fragment it still holds to be half
/// rewritten.
pub fn rewrite_video_samples<F>(
    fragment: &mut Fragment,
    tracks: &[Track],
    video_track: u32,
    mut rewrite: F,
) -> Result<bool, Fmp4Error>
where
    F: FnMut(&[u8]) -> Result<Vec<u8>, Fmp4Error>,
{
    let has_default_sizes = fragment
        .track(video_track)
        .is_some_and(|track| track.samples().any(|sample| sample.size_at.is_none()));
    if has_default_sizes {
        // Work on a clone so materializing the trun fields remains part of the
        // rewrite's all-or-nothing promise. A later closure error must not
        // leave the caller with a structurally changed fragment.
        let mut candidate = fragment.clone();
        materialize_video_sample_sizes(&mut candidate, tracks, video_track)?;
        let changed =
            rewrite_video_samples_explicit(&mut candidate, tracks, video_track, &mut rewrite)?;
        if changed {
            *fragment = candidate;
        }
        return Ok(changed);
    }

    rewrite_video_samples_explicit(fragment, tracks, video_track, &mut rewrite)
}

fn rewrite_video_samples_explicit<F>(
    fragment: &mut Fragment,
    tracks: &[Track],
    video_track: u32,
    rewrite: &mut F,
) -> Result<bool, Fmp4Error>
where
    F: FnMut(&[u8]) -> Result<Vec<u8>, Fmp4Error>,
{
    // Collect the edits before applying any, so a refusal partway through
    // leaves the fragment exactly as it was. A half-rewritten fragment is a
    // stream that decodes to garbage — and one that still parses, so nothing
    // downstream would notice.
    struct Edit {
        at: Range<usize>,
        replacement: Vec<u8>,
        size_at: usize,
    }
    let mut edits: Vec<Edit> = Vec::new();
    for track in &fragment.tracks {
        if track.track_id != video_track {
            continue;
        }
        for run in &track.runs {
            let mut at = run.data_offset;
            for sample in &run.samples {
                let size = sample.size as usize;
                let end = at
                    .checked_add(size)
                    .filter(|end| *end <= fragment.bytes.len());
                let Some(end) = end else {
                    return Err(Fmp4Error::Unsupported(
                        "a sample runs past the end of its fragment".into(),
                    ));
                };
                let Some(size_at) = sample.size_at else {
                    return Err(Fmp4Error::Unsupported(
                        "a video sample size was not materialized before rewrite".into(),
                    ));
                };
                let replacement = rewrite(&fragment.bytes[at..end])?;
                // Recorded whenever the bytes differ at all, not only when the
                // length does: a same-length rewrite still has to be written,
                // and answering "nothing changed" for one would leave the
                // caller believing a conversion happened that did not.
                if replacement != fragment.bytes[at..end] {
                    edits.push(Edit {
                        at: at..end,
                        replacement,
                        size_at,
                    });
                }
                at = end;
            }
        }
    }
    if edits.is_empty() {
        return Ok(false);
    }

    // Every run that begins at or after a shortened sample moves. Gather the
    // declarations first, for the same reason as above.
    let mut offsets: Vec<(usize, usize)> = Vec::new();
    for track in &fragment.tracks {
        for (index, run) in track.runs.iter().enumerate() {
            match run.data_offset_at {
                Some(field) => offsets.push((run.data_offset, field)),
                // The first run of a traf inherits from nothing, so an absent
                // field there is `data_offset` zero and needs no correction.
                // A later one inherits from the run before it, whose samples
                // this rewrite is resizing — its position would silently
                // follow bytes that moved.
                None if index > 0 => {
                    return Err(Fmp4Error::Unsupported(
                        "a trun continues from the previous run's end, which a per-sample \
                         rewrite moves"
                            .into(),
                    ))
                }
                None => {}
            }
        }
    }

    // Everything that can fail is decided here, before a single byte moves.
    //
    // The atomicity above is the invariant every caller's safety is argued
    // from — `vodgen` fails the generation on a refusal, `fragindex` returns
    // `Unsupported`, and neither expects the fragment it still holds to be
    // half-rewritten. Collecting the edits was only half of keeping that
    // promise: an `Unsupported` raised while applying them would have left the
    // bytes spliced and the sizes not yet corrected.
    edits.sort_by_key(|edit| edit.at.start);
    let mut previous_end = 0usize;
    for edit in &edits {
        if edit.at.start < previous_end {
            return Err(Fmp4Error::Unsupported(
                "two samples of this fragment overlap, so their rewrites cannot both be                  applied"
                    .into(),
            ));
        }
        previous_end = edit.at.end;
    }
    let first_edit = edits[0].at.start;

    // Every size and offset this writes lives in the `moof`, and every edit is
    // in the `mdat` payload behind it — which is what lets the fields be
    // written at the offsets they were parsed at even though the bytes after
    // them moved. Checked rather than assumed: a field that landed inside the
    // rewritten region would be written at a position the rewrite had already
    // changed the meaning of.
    let mut writes: Vec<(usize, u32)> = Vec::with_capacity(edits.len() + offsets.len());
    let mut delta_at: Vec<(usize, i64)> = Vec::with_capacity(edits.len());
    for edit in &edits {
        let before = edit.at.end - edit.at.start;
        let after = edit.replacement.len();
        let size = u32::try_from(after)
            .map_err(|_| Fmp4Error::Unsupported("a rewritten sample is absurdly long".into()))?;
        writes.push((edit.size_at, size));
        delta_at.push((edit.at.start, after as i64 - before as i64));
    }

    // Then the offsets, each moved by everything that changed length before
    // it. `data_offset` is relative to the fragment's own start, which is
    // exactly the coordinate the edits were recorded in.
    for (offset, field) in offsets {
        let shift: i64 = delta_at
            .iter()
            .filter(|(at, _)| *at < offset)
            .map(|(_, delta)| *delta)
            .sum();
        if shift == 0 {
            continue;
        }
        let moved = i64::try_from(offset)
            .ok()
            .and_then(|offset| offset.checked_add(shift))
            .and_then(|moved| u32::try_from(moved).ok())
            .ok_or_else(|| {
                Fmp4Error::Unsupported("a corrected trun data_offset does not fit".into())
            })?;
        writes.push((field, moved));
    }

    // And the `mdat`, which is the only box whose size changes.
    let total: i64 = delta_at.iter().map(|(_, delta)| *delta).sum();
    let header = fragment.mdat_payload.start;
    // Read the width rather than assuming eight. A `largesize` `mdat` states
    // its length in a 64-bit field past the box name, so `header - 8` would
    // land on the high half of it — zero for anything under 4 GiB, which then
    // reads as the "size is elsewhere" sentinel and refuses by luck rather
    // than by decision. Refuse it by name instead: this rewrite writes a
    // 32-bit size, and there is nowhere in a `largesize` box to write one.
    let mdat_header = mdat_header_len(&fragment.bytes, header)?;
    if mdat_header != 8 {
        return Err(Fmp4Error::Unsupported(format!(
            "an mdat with a {mdat_header}-byte header states its size in a field this              rewrite cannot correct"
        )));
    }
    let old = be_u32(&fragment.bytes, header - 8) as i64;
    if old == 0 || old == 1 {
        return Err(Fmp4Error::Unsupported(
            "an mdat with an extended or to-end-of-file size cannot be resized in place".into(),
        ));
    }
    let new = u32::try_from(old + total)
        .map_err(|_| Fmp4Error::Unsupported("a corrected mdat size does not fit".into()))?;
    writes.push((header - 8, new));
    let payload_end = usize::try_from(fragment.mdat_payload.end as i64 + total).map_err(|_| {
        Fmp4Error::Unsupported("a corrected mdat payload range does not fit".into())
    })?;

    for (field, _) in &writes {
        if field.saturating_add(4) > first_edit {
            return Err(Fmp4Error::Unsupported(
                "a size this rewrite must correct sits inside the bytes it rewrites".into(),
            ));
        }
    }

    // Built forward into a fresh buffer rather than spliced in place: one pass
    // instead of one memmove per edit, and the fragment stays untouched until
    // the re-parse below has proved the result readable.
    let mut bytes = Vec::with_capacity(fragment.bytes.len());
    let mut copied = 0usize;
    for edit in &edits {
        bytes.extend_from_slice(&fragment.bytes[copied..edit.at.start]);
        bytes.extend_from_slice(&edit.replacement);
        copied = edit.at.end;
    }
    bytes.extend_from_slice(&fragment.bytes[copied..]);
    for (field, value) in writes {
        write_be_u32(&mut bytes, field, value)?;
    }

    // Re-parse rather than adjusting the model by hand: the bytes are the
    // truth now, and a model patched in parallel is a model that can disagree
    // with them.
    //
    // Not optional, and the first version of this function skipped it while
    // saying this. Every reader downstream — the segmenter's `merge`, the
    // landing matcher's byte counts, `classify`, the promotion capture —
    // addresses media through `tracks`, so a stale model does not read
    // slightly wrong samples, it reads them from offsets that no longer
    // exist. The audio run, which sits after the video the rewrite shortened,
    // lands past the end of `bytes`, and every converting session fails at its
    // first segment.
    let header_len = moof_header_len(&bytes)?;
    let moof_len = fragment.mdat_payload.start - mdat_header;
    let parsed = parse_moof(&bytes[..moof_len], header_len, tracks)?;

    fragment.bytes = bytes;
    fragment.mdat_payload = fragment.mdat_payload.start..payload_end;
    fragment.tracks = parsed;
    Ok(true)
}

/// Give a video `trun` that relies on `tfhd.default_sample_size` an explicit
/// size for every sample.
///
/// The inserted fields grow `trun`, `traf`, and `moof`; because media offsets
/// are relative to the beginning of `moof`, every explicit `data_offset` in
/// every track moves by the same amount. Unknown sibling boxes are copied
/// byte-for-byte so auxiliary metadata is not lost merely because the video
/// run used a compact legal encoding.
fn materialize_video_sample_sizes(
    fragment: &mut Fragment,
    tracks: &[Track],
    video_track: u32,
) -> Result<bool, Fmp4Error> {
    let Some(video) = fragment.track(video_track) else {
        return Ok(false);
    };
    let missing = video
        .runs
        .iter()
        .flat_map(|run| &run.samples)
        .filter(|sample| sample.size_at.is_none())
        .count();
    if missing == 0 {
        return Ok(false);
    }
    let growth = missing.checked_mul(4).ok_or_else(|| {
        Fmp4Error::Unsupported("explicit video sample sizes make the moof too large".into())
    })?;

    let moof_header = peek_box(&fragment.bytes, 0)?
        .ok_or_else(|| Fmp4Error::Unsupported("a fragment with no complete moof".into()))?;
    if moof_header.kind() != b"moof" {
        return Err(Fmp4Error::Unsupported(
            "a fragment that does not begin with a moof".into(),
        ));
    }
    let old_moof_len = moof_header.size;
    let moof_payload = fragment
        .bytes
        .get(moof_header.header_len..old_moof_len)
        .ok_or_else(|| Fmp4Error::Malformed("moof runs past its fragment".into()))?;

    let mut rebuilt_payload = Vec::with_capacity(moof_payload.len() + growth);
    let mut copied = 0usize;
    for (header, start, end) in children(moof_payload)? {
        let box_start = start - header.header_len;
        rebuilt_payload.extend_from_slice(&moof_payload[copied..box_start]);
        if header.kind() == b"traf" {
            let payload = &moof_payload[start..end];
            let track_id = traf_track_id(payload)?;
            let runs = fragment
                .track(track_id)
                .map(|track| track.runs.as_slice())
                .unwrap_or(&[]);
            let rebuilt = rebuild_traf_with_sample_sizes(
                header,
                payload,
                runs,
                track_id == video_track,
                growth,
            )?;
            rebuilt_payload.extend_from_slice(&rebuilt);
        } else {
            rebuilt_payload.extend_from_slice(&moof_payload[box_start..end]);
        }
        copied = end;
    }
    rebuilt_payload.extend_from_slice(&moof_payload[copied..]);
    let rebuilt_moof = wrap_box(moof_header, &rebuilt_payload)?;
    if rebuilt_moof.len() != old_moof_len + growth {
        return Err(Fmp4Error::Unsupported(
            "materialized video sample sizes changed the moof by an unexpected amount".into(),
        ));
    }

    let mut bytes = Vec::with_capacity(fragment.bytes.len() + growth);
    bytes.extend_from_slice(&rebuilt_moof);
    bytes.extend_from_slice(&fragment.bytes[old_moof_len..]);
    let payload_start = fragment
        .mdat_payload
        .start
        .checked_add(growth)
        .ok_or_else(|| Fmp4Error::Unsupported("moved mdat start does not fit".into()))?;
    let payload_end = fragment
        .mdat_payload
        .end
        .checked_add(growth)
        .ok_or_else(|| Fmp4Error::Unsupported("moved mdat end does not fit".into()))?;
    let header_len = moof_header_len(&bytes)?;
    let parsed = parse_moof(&bytes[..rebuilt_moof.len()], header_len, tracks)?;

    fragment.bytes = bytes;
    fragment.mdat_payload = payload_start..payload_end;
    fragment.tracks = parsed;
    Ok(true)
}

fn traf_track_id(payload: &[u8]) -> Result<u32, Fmp4Error> {
    for (header, start, end) in children(payload)? {
        if header.kind() != b"tfhd" {
            continue;
        }
        let body = &payload[start..end];
        if body.len() < 8 {
            return malformed("tfhd too short");
        }
        return Ok(be_u32(body, 4));
    }
    malformed("traf has no tfhd track_id")
}

fn rebuild_traf_with_sample_sizes(
    header: BoxHeader,
    payload: &[u8],
    runs: &[Run],
    materialize: bool,
    moof_growth: usize,
) -> Result<Vec<u8>, Fmp4Error> {
    let mut rebuilt = Vec::new();
    let mut copied = 0usize;
    let mut run_index = 0usize;
    for (child, start, end) in children(payload)? {
        let box_start = start - child.header_len;
        rebuilt.extend_from_slice(&payload[copied..box_start]);
        if child.kind() == b"trun" {
            let run = runs
                .get(run_index)
                .ok_or_else(|| Fmp4Error::Malformed("parsed traf and trun list disagree".into()))?;
            let trun = rebuild_trun_with_sample_sizes(
                child,
                &payload[start..end],
                run,
                materialize,
                moof_growth,
            )?;
            rebuilt.extend_from_slice(&trun);
            run_index += 1;
        } else {
            rebuilt.extend_from_slice(&payload[box_start..end]);
        }
        copied = end;
    }
    rebuilt.extend_from_slice(&payload[copied..]);
    if run_index != runs.len() {
        return Err(Fmp4Error::Malformed(
            "parsed traf and trun list disagree".into(),
        ));
    }
    wrap_box(header, &rebuilt)
}

fn rebuild_trun_with_sample_sizes(
    header: BoxHeader,
    payload: &[u8],
    run: &Run,
    materialize: bool,
    moof_growth: usize,
) -> Result<Vec<u8>, Fmp4Error> {
    if payload.len() < 8 {
        return malformed("trun too short");
    }
    let flags = be_u32(payload, 0) & 0x00ff_ffff;
    let count = be_u32(payload, 4) as usize;
    if count != run.samples.len() {
        return Err(Fmp4Error::Malformed(
            "parsed run and trun sample counts disagree".into(),
        ));
    }
    let has_sizes = flags & 0x00_0200 != 0;
    let should_materialize = materialize && !has_sizes;
    if materialize
        && run
            .samples
            .iter()
            .any(|sample| sample.size_at.is_none() == has_sizes)
    {
        return Err(Fmp4Error::Malformed(
            "parsed sample-size fields disagree with their trun".into(),
        ));
    }

    let mut source = payload.to_vec();
    let mut p = 8usize;
    if flags & 0x00_0001 != 0 {
        if source.len() < p + 4 {
            return malformed("trun truncated at data_offset");
        }
        let old = be_u32(&source, p) as i32;
        if old < 0 {
            return Err(Fmp4Error::Unsupported(
                "trun with a negative data_offset".into(),
            ));
        }
        let moved = i64::from(old)
            .checked_add(moof_growth as i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| {
                Fmp4Error::Unsupported("a moved trun data_offset does not fit".into())
            })?;
        source[p..p + 4].copy_from_slice(&moved.to_be_bytes());
        p += 4;
    }
    if flags & 0x00_0004 != 0 {
        p += 4;
    }
    if source.len() < p {
        return malformed("trun truncated before its samples");
    }
    if !should_materialize {
        return wrap_box(header, &source);
    }

    let per = 4 * usize::from(flags & 0x00_0100 != 0)
        + 4 * usize::from(flags & 0x00_0200 != 0)
        + 4 * usize::from(flags & 0x00_0400 != 0)
        + 4 * usize::from(flags & 0x00_0800 != 0);
    let samples_end = p
        .checked_add(count.saturating_mul(per))
        .filter(|end| *end <= source.len())
        .ok_or_else(|| Fmp4Error::Malformed("trun samples run past their box".into()))?;
    let mut rebuilt = Vec::with_capacity(source.len() + count * 4);
    rebuilt.extend_from_slice(&source[..p]);
    let full_flags = be_u32(&rebuilt, 0) | 0x0000_0200;
    rebuilt[0..4].copy_from_slice(&full_flags.to_be_bytes());
    let mut at = p;
    for sample in &run.samples {
        if flags & 0x00_0100 != 0 {
            rebuilt.extend_from_slice(&source[at..at + 4]);
            at += 4;
        }
        rebuilt.extend_from_slice(&sample.size.to_be_bytes());
        if flags & 0x00_0400 != 0 {
            rebuilt.extend_from_slice(&source[at..at + 4]);
            at += 4;
        }
        if flags & 0x00_0800 != 0 {
            rebuilt.extend_from_slice(&source[at..at + 4]);
            at += 4;
        }
    }
    debug_assert_eq!(at, samples_end);
    rebuilt.extend_from_slice(&source[samples_end..]);
    wrap_box(header, &rebuilt)
}

fn wrap_box(header: BoxHeader, payload: &[u8]) -> Result<Vec<u8>, Fmp4Error> {
    let size = header
        .header_len
        .checked_add(payload.len())
        .ok_or_else(|| Fmp4Error::Unsupported("rebuilt box size does not fit".into()))?;
    let mut bytes = Vec::with_capacity(size);
    match header.header_len {
        8 => {
            let size = u32::try_from(size)
                .map_err(|_| Fmp4Error::Unsupported("rebuilt box is too large".into()))?;
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(header.kind());
        }
        16 => {
            bytes.extend_from_slice(&1u32.to_be_bytes());
            bytes.extend_from_slice(header.kind());
            bytes.extend_from_slice(&(size as u64).to_be_bytes());
        }
        width => {
            return Err(Fmp4Error::Unsupported(format!(
                "cannot rebuild a box with a {width}-byte header"
            )));
        }
    }
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

/// The `mdat` box header width implied by where its payload begins.
///
/// A `largesize` box carries sixteen bytes of header rather than eight, and
/// the difference is what separates the `moof`'s length from the payload's
/// start.
fn mdat_header_len(bytes: &[u8], payload_start: usize) -> Result<usize, Fmp4Error> {
    for width in [8usize, 16] {
        let Some(name) = payload_start
            .checked_sub(width)
            .and_then(|at| bytes.get(at + 4..at + 8))
        else {
            continue;
        };
        if name == b"mdat" {
            return Ok(width);
        }
    }
    Err(Fmp4Error::Unsupported(
        "the mdat payload does not begin after an mdat header".into(),
    ))
}

fn moof_header_len(bytes: &[u8]) -> Result<usize, Fmp4Error> {
    let hdr = peek_box(bytes, 0)?
        .ok_or_else(|| Fmp4Error::Unsupported("a fragment with no leading box".into()))?;
    if hdr.kind() != b"moof" {
        return Err(Fmp4Error::Unsupported(
            "a fragment that does not begin with a moof".into(),
        ));
    }
    Ok(hdr.header_len)
}

fn write_be_u32(bytes: &mut [u8], at: usize, value: u32) -> Result<(), Fmp4Error> {
    let end = at
        .checked_add(4)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| Fmp4Error::Unsupported("a field to patch lies outside the box".into()))?;
    bytes[at..end].copy_from_slice(&value.to_be_bytes());
    Ok(())
}

#[path = "fmp4/dvh1.rs"]
mod dvh1;
pub use dvh1::validate_rolling_dvh1_fragment;

#[path = "fmp4/hdr10_index.rs"]
mod hdr10_index;
pub use hdr10_index::{
    normalize_indexed_hdr10_metadata, strip_sdr_hdr_fragment, strip_sdr_hdr_init,
    Hdr10IndexMetadata, Hdr10OutputMetadata, HDR10_INDEX_METADATA_REVISION,
};

/// Promote an init from captured inputs — the planned generation's path.
///
/// Answers whether anything changed. Applying this to a muxer init that is
/// byte-identical across generations yields a served init that is
/// byte-identical across generations, which is the whole point: a client
/// holding a playlist from one generation is served media from another, and
/// the init it fetched has to describe both.
pub fn promote_from(init: &mut Init, inputs: &PromotionInputs) -> Result<bool, Fmp4Error> {
    // Before anything mutates the init. The refusal exists to catch a caller
    // bug, and a promotion that appends parameter sets and HDR10 metadata and
    // *then* refuses has already changed the bytes it was asked not to build —
    // so the caller's error path discards an init that is no longer the
    // muxer's, and a reader comparing digests sees drift it cannot explain.
    if inputs.dolby_vision.is_some() && inputs.strip_dolby_vision {
        return Err(Fmp4Error::Unsupported(
            "the promotion inputs ask to both rewrite and remove the Dolby Vision \
             record; one of those callers is wrong and picking would hide it"
                .into(),
        ));
    }
    if inputs.dvh1_parameter_sets_malformed {
        return malformed("dvh1 opening parameter-set evidence is malformed");
    }
    // Before the first mutator: each one refuses an init with two
    // descriptions, and this removes the repeated one ffmpeg 8 writes.
    let collapsed = collapse_equivalent_hevc_sample_entries(init)? > 0;
    let dvh1 = dvh1::complete(init, &inputs.dvh1_parameter_sets)?;
    let hevc = promote_hevc_parameter_sets_from(init, &inputs.parameter_sets)?;
    // The removal goes FIRST, and the rewrite below goes last, for the same
    // reason pointing opposite ways: `promote_hdr10_static_metadata_from`
    // refuses to run on a sample entry that carries a Dolby Vision record.
    //
    // A stripped stream is an HDR10 stream — that is the entire claim the
    // strip makes — and its mastering-display metadata is exactly as worth
    // promoting as any other HDR10 title's. Removing the record afterwards
    // would leave the promotion inhibited by a record that is about to be
    // deleted, and the one stream this whole change exists to fix would be the
    // one served without its HDR10 static metadata.
    let removed = if inputs.strip_dolby_vision {
        remove_dolby_vision_record(init)?
    } else {
        false
    };
    let content_light =
        inputs.indexed_hdr10_metadata && normalize_indexed_hdr10_metadata(init, &inputs.hdr10_sei)?;
    let hdr10 = promote_hdr10_static_metadata_from(init, &inputs.hdr10_sei)?;
    // Last, and deliberately so. The HDR10 promotion above refuses to run on a
    // sample entry that already carries a Dolby Vision configuration, and
    // writing the record first would make it refuse on the one stream that
    // most needs it — a converted 8.1 stream over an HDR10 base, whose
    // mastering-display metadata is exactly as worth promoting as any other
    // HDR10 title's.
    // The contradiction is already refused above, before the first mutation.
    let dolby_vision = match inputs.dolby_vision.as_ref() {
        Some(record) => set_dolby_vision_record(init, record)?,
        // Already done above, before the HDR10 promotion it would otherwise
        // inhibit. Carried through so the "did anything change" answer is
        // still the union of all three.
        None => removed,
    };
    Ok(collapsed || dvh1 || hevc || content_light || hdr10 || dolby_vision)
}

pub fn promote_hevc_parameter_sets(init: &mut Init, first: &Fragment) -> Result<bool, Fmp4Error> {
    let collapsed = collapse_equivalent_hevc_sample_entries(init)? > 0;
    let inputs = PromotionInputs::from_fragment(first, init);
    if inputs.dvh1_parameter_sets_malformed {
        return malformed("dvh1 opening parameter-set evidence is malformed");
    }
    let dvh1 = dvh1::complete(init, &inputs.dvh1_parameter_sets)?;
    Ok(promote_hevc_parameter_sets_from(init, &inputs.parameter_sets)? || dvh1 || collapsed)
}

/// The same surgery, from parameter sets captured somewhere else.
///
/// This is the form a planned generation uses. See [`PromotionInputs`] for why
/// a repositioned generation must not read its own landing fragment.
pub fn promote_hevc_parameter_sets_from(
    init: &mut Init,
    parameter_sets: &[Vec<u8>],
) -> Result<bool, Fmp4Error> {
    let Some(video) = init.video() else {
        return Ok(false);
    };
    if video.codec != Some(VideoCodec::Hevc) || video.nal_length_size == 0 {
        return Ok(false);
    }

    let candidate_nals: Vec<&[u8]> = parameter_sets.iter().map(Vec::as_slice).collect();
    if candidate_nals.is_empty() {
        return Ok(false);
    }

    let Some(location) = locate_hvcc(&init.bytes)? else {
        return Err(Fmp4Error::Unsupported(
            "the HEVC video sample entry has no hvcC box to enrich".into(),
        ));
    };
    let record = &init.bytes[location.payload.clone()];
    let present = hvcc_nal_array_types(record)?;
    let mut appended = Vec::new();
    let mut added_arrays = 0u8;
    for kind in 32u8..=34 {
        if present.contains(&kind) {
            continue;
        }
        let mut nals: Vec<&[u8]> = candidate_nals
            .iter()
            .copied()
            .filter(|nal| hevc_nal_type(nal) == Some(kind))
            .collect();
        nals.dedup();
        if nals.is_empty() {
            continue;
        }
        let count = u16::try_from(nals.len()).map_err(|_| {
            Fmp4Error::Unsupported(format!("too many HEVC type-{kind} parameter sets for hvcC"))
        })?;
        // array_completeness=1: every parameter set of this type found in the
        // authored opening access unit is carried in this sample entry.
        appended.push(0x80 | kind);
        appended.extend_from_slice(&count.to_be_bytes());
        for nal in nals {
            let len = u16::try_from(nal.len()).map_err(|_| {
                Fmp4Error::Unsupported(format!(
                    "an HEVC type-{kind} parameter set exceeds hvcC's u16 length"
                ))
            })?;
            appended.extend_from_slice(&len.to_be_bytes());
            appended.extend_from_slice(nal);
        }
        added_arrays = added_arrays.checked_add(1).ok_or_else(|| {
            Fmp4Error::Unsupported("too many HEVC parameter-set arrays for hvcC".into())
        })?;
    }
    if appended.is_empty() {
        return Ok(false);
    }

    // Promotion is an initialization repair, not a best-effort metadata
    // copy. A hvc1/dvh1 decoder configuration missing even one of VPS, SPS,
    // or PPS is still unusable, and mutating it partway would let callers
    // publish an init that looks enriched while remaining undecodable.
    let complete = (32u8..=34).all(|kind| {
        present.contains(&kind)
            || candidate_nals
                .iter()
                .any(|nal| hevc_nal_type(nal) == Some(kind))
    });
    if !complete {
        return Err(Fmp4Error::Unsupported(
            "HEVC init promotion requires a complete VPS/SPS/PPS set".into(),
        ));
    }

    let arrays_offset = location.payload.start + 22;
    let new_count = init.bytes[arrays_offset]
        .checked_add(added_arrays)
        .ok_or_else(|| Fmp4Error::Unsupported("hvcC already declares 255 NAL arrays".into()))?;
    let delta = appended.len();
    init.bytes
        .splice(location.payload.end..location.payload.end, appended);
    init.bytes[arrays_offset] = new_count;
    for &box_at in &location.ancestors {
        grow_box(&mut init.bytes, box_at, delta)?;
    }
    Ok(true)
}

/// A Dolby Vision decoder configuration record, as it sits in a sample entry.
///
/// The five fields anything in plurx reads, **plus the bytes it does not**.
/// The record is 24 bytes and this type understands five of them; the rest is
/// reserved space that later spec revisions have already started using —
/// `dv_md_compression` lives in bits 2-3 of byte 4 in ISO spec v2.1, and the
/// ffmpeg that ships in the image writes it. So the original bytes are carried
/// and a rewrite patches the fields in place rather than rebuilding from zero.
/// Rebuilding would silently tell a decoder that compressed RPU metadata is
/// uncompressed, and nothing anywhere would report that bytes had been
/// dropped.
///
/// The box has two names for one payload: `dvcC` for profiles up to 7 and
/// `dvvC` for 8 and above.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DolbyVisionRecord {
    /// 5, 7, 8, 9, 10 — what the stream actually is.
    pub profile: u8,
    /// The level. This, with the profile, is what the HLS master playlist's
    /// `dvh1.PP.LL` suffix is built from (`dolby_vision_codec_from_init`).
    pub level: u8,
    /// Whether an enhancement layer is present.
    ///
    /// Read and written faithfully, and never inferred. plurx's own Dolby
    /// Vision strip does not produce a record at all — `dovi_rpu=strip=1`
    /// removes the side data, so the output is signalled as plain HDR10 — so
    /// every record this type sees is one a muxer wrote from a source that
    /// had one, and its answer here is that source's answer.
    pub el_present: bool,
    /// Whether an RPU is present.
    pub rpu_present: bool,
    /// Whether a base layer is present — true in every real stream.
    pub bl_present: bool,
    /// `dv_bl_signal_compatibility_id`: 1/6 = HDR10 base, 4 = HLG, 2 = SDR,
    /// 0 = none. Not part of the playlist's codec string; it is what a client
    /// reads to decide whether the base layer alone is watchable.
    pub bl_signal_compatibility_id: u8,
    /// Every byte of the record as it was read, so a rewrite can preserve what
    /// this type does not model. Empty for a record built from scratch, which
    /// is the conversion's case: there is nothing to preserve.
    raw: Vec<u8>,
}

/// Two records are equal when they *say* the same thing.
///
/// `raw` is carried so a rewrite can preserve bytes this type does not model,
/// not because it is part of the configuration. Comparing it would make a
/// record built from scratch unequal to the identical record read back out of
/// an init, which is exactly the comparison every caller wants to make.
impl PartialEq for DolbyVisionRecord {
    fn eq(&self, other: &DolbyVisionRecord) -> bool {
        (
            self.profile,
            self.level,
            self.el_present,
            self.rpu_present,
            self.bl_present,
            self.bl_signal_compatibility_id,
        ) == (
            other.profile,
            other.level,
            other.el_present,
            other.rpu_present,
            other.bl_present,
            other.bl_signal_compatibility_id,
        )
    }
}

impl Eq for DolbyVisionRecord {}

/// A record whose fields do not fit the bits the format gives them.
fn out_of_range<T>(field: &str, value: u8, max: u8) -> Result<T, Fmp4Error> {
    Err(Fmp4Error::Unsupported(format!(
        "Dolby Vision {field} {value} does not fit in the record (max {max})"
    )))
}

impl DolbyVisionRecord {
    /// Build a record from scratch — the conversion's case.
    ///
    /// Refuses rather than truncates. A writer whose output is the contract
    /// with every Dolby Vision decoder downstream must not quietly turn
    /// profile 128 into profile 0, which is what masking would do — and
    /// `dolby_vision_codec_from_init` then rejects profile 0 and falls back to
    /// an `hvcC`-derived codec string that AVPlayer refuses on a DV sample
    /// entry.
    pub fn new(
        profile: u8,
        level: u8,
        el_present: bool,
        rpu_present: bool,
        bl_present: bool,
        bl_signal_compatibility_id: u8,
    ) -> Result<DolbyVisionRecord, Fmp4Error> {
        if profile == 0 || profile > 0x7f {
            return out_of_range("profile", profile, 0x7f);
        }
        if level == 0 || level > 0x3f {
            return out_of_range("level", level, 0x3f);
        }
        if bl_signal_compatibility_id > 0x0f {
            return out_of_range(
                "base-layer compatibility id",
                bl_signal_compatibility_id,
                0x0f,
            );
        }
        Ok(DolbyVisionRecord {
            profile,
            level,
            el_present,
            rpu_present,
            bl_present,
            bl_signal_compatibility_id,
            raw: Vec::new(),
        })
    }

    /// The box name for this profile: `dvvC` at 8 and above, `dvcC` below.
    ///
    /// This follows ffmpeg's `movenc` rule (`dv_profile > 7`), deliberately:
    /// ffmpeg is the muxer that writes every stream plurx serves, so matching
    /// it is what makes a plurx-written record indistinguishable from one
    /// ffmpeg wrote for the same stream.
    pub fn box_name(&self) -> [u8; 4] {
        if self.profile >= 8 {
            *b"dvvC"
        } else {
            *b"dvcC"
        }
    }

    /// Parse a record, keeping every byte.
    ///
    /// Bit layout, big-endian across bytes 2-3 (bytes 0-1 are the version
    /// pair): `profile:7 | level:6 | rpu_present:1 | el_present:1 |
    /// bl_present:1`. The compatibility id is the high nibble of byte 4.
    /// Everything after is reserved — not necessarily zero, and not this
    /// type's to discard.
    fn parse(payload: &[u8]) -> Result<DolbyVisionRecord, Fmp4Error> {
        if payload.len() < 24 {
            return malformed("Dolby Vision configuration record is shorter than 24 bytes");
        }
        let packed = u16::from_be_bytes([payload[2], payload[3]]);
        Ok(DolbyVisionRecord {
            profile: ((packed >> 9) & 0x7f) as u8,
            level: ((packed >> 3) & 0x3f) as u8,
            rpu_present: (packed >> 2) & 1 == 1,
            el_present: (packed >> 1) & 1 == 1,
            bl_present: packed & 1 == 1,
            bl_signal_compatibility_id: (payload[4] >> 4) & 0x0f,
            raw: payload.to_vec(),
        })
    }

    /// The payload to write.
    ///
    /// A record that was read patches its own bytes; one built from scratch
    /// starts at 24 zeroes with `dv_version_major` 1 and `dv_version_minor` 0,
    /// which is what every consumer stream carries and what ffmpeg writes.
    fn to_payload(&self) -> Vec<u8> {
        let mut out = if self.raw.is_empty() {
            let mut fresh = vec![0u8; 24];
            fresh[0] = 1;
            fresh
        } else {
            self.raw.clone()
        };
        let packed = (u16::from(self.profile & 0x7f) << 9)
            | (u16::from(self.level & 0x3f) << 3)
            | (u16::from(self.rpu_present) << 2)
            | (u16::from(self.el_present) << 1)
            | u16::from(self.bl_present);
        out[2..4].copy_from_slice(&packed.to_be_bytes());
        // High nibble only: the low nibble is `dv_md_compression` and reserved
        // bits that this type does not model and must not clear.
        out[4] = (out[4] & 0x0f) | ((self.bl_signal_compatibility_id & 0x0f) << 4);
        out
    }

    /// The same record with its enhancement-layer claim told the truth.
    ///
    /// The strip-path correction, as a value rather than a mutation, so a
    /// caller can compare it against what it read and write only on a change.
    pub fn without_enhancement_layer(&self) -> DolbyVisionRecord {
        DolbyVisionRecord {
            el_present: false,
            ..self.clone()
        }
    }
}

/// The Dolby Vision configuration this init declares, if it declares one.
pub fn dolby_vision_record(init: &Init) -> Result<Option<DolbyVisionRecord>, Fmp4Error> {
    let Some(location) = locate_configuring_hvcc(&init.bytes)? else {
        return Ok(None);
    };
    let Some(existing) = location.dolby_vision else {
        return Ok(None);
    };
    DolbyVisionRecord::parse(&init.bytes[existing.payload]).map(Some)
}

/// A packet-preserving author keeps any in-band VPS/SPS/PPS. Declare that
/// representation explicitly rather than inheriting an out-of-band promise.
pub fn declare_hevc_parameter_sets_in_band(init: &mut Init) -> Result<(), Fmp4Error> {
    let locations = locate_hevc_sample_entries(&init.bytes)?;
    if locations.len() != 1 {
        return malformed("one HEVC sample entry required");
    }
    let location = locations
        .into_iter()
        .next()
        .flatten()
        .ok_or_else(|| Fmp4Error::Malformed("no HEVC sample entry".into()))?;
    let entry = location.ancestors[1];
    init.bytes[entry.start + 4..entry.start + 8].copy_from_slice(b"hev1");
    Ok(())
}

/// Write `record` into this init's video sample entry, replacing whatever
/// configuration was there.
///
/// One caller today, one contingency (M5a-0, `docs/streaming/PLAYBACK-CAPS-V2-M0.md` §8):
///
/// - **The conversion.** ffmpeg does not derive this record from the RPU — it
///   copies the one the input container had. The P7→P8.1 rewrite runs on the
///   far side of that muxer, so its output carries the *source's* Profile 7
///   `dvcC` over samples whose RPUs now say 8.1: a sample entry declaring an
///   enhancement layer that is no longer in the stream, which is worse than
///   no record at all. This writer replaces it — the payload is 24 bytes in
///   both spellings, so the box changes name and contents and nothing
///   resizes. It appends instead on the shape where the muxer wrote none,
///   which is why both branches exist.
/// - **A correction**, should a stream ever be found whose record disagrees
///   with its samples in some other way. Nothing does today: plurx's strip
///   removes the side data outright rather than leaving a stale record, and
///   its preserve keeps both the layers and the record that describes them.
///
/// Answers whether the init changed. Writing a record byte-identical to the
/// one already there — under the same box name — is a no-op, and that is not a
/// nicety: `InitIdentity` digests the served init and refuses a later
/// generation that does not hash the same, so a write that "corrects" nothing
/// would break every generation after the first.
pub fn set_dolby_vision_record(
    init: &mut Init,
    record: &DolbyVisionRecord,
) -> Result<bool, Fmp4Error> {
    let Some(location) = locate_hvcc(&init.bytes)? else {
        return Err(Fmp4Error::Unsupported(
            "no HEVC video sample entry to carry a Dolby Vision record".into(),
        ));
    };
    let name = record.box_name();

    let changed = if let Some(existing) = location.dolby_vision {
        let payload = record.to_payload();
        if existing.payload.len() != payload.len() {
            return Err(Fmp4Error::Unsupported(format!(
                "the existing Dolby Vision record is {} bytes and the replacement is {}; \
                 a rewrite must not resize it",
                existing.payload.len(),
                payload.len()
            )));
        }
        if existing.name == name && init.bytes[existing.payload.clone()] == payload[..] {
            return Ok(false);
        }
        // The payload is the same length in both spellings, so an in-place
        // rewrite changes no size anywhere — including when only the box's
        // NAME changes, which is what a profile 7 record becoming a profile 8
        // one does. The name sits four bytes before the payload only in an
        // 8-byte header; a `largesize` box puts the length there instead, and
        // writing the name over it produces an init that no longer parses.
        let name_at = existing.payload.start - existing.header_len + 4;
        init.bytes[name_at..name_at + 4].copy_from_slice(&name);
        init.bytes[existing.payload].copy_from_slice(&payload);
        true
    } else {
        // No record at all: append one at the end of the sample entry, and
        // grow every box that contains it. `sample_entry_end` is the end of
        // the entry's child boxes, so this lands after hvcC and any mdcv/clli.
        let payload = record.to_payload();
        let mut boxed = Vec::with_capacity(8 + payload.len());
        let size = u32::try_from(8 + payload.len())
            .map_err(|_| Fmp4Error::Unsupported("Dolby Vision record is absurdly long".into()))?;
        boxed.extend_from_slice(&size.to_be_bytes());
        boxed.extend_from_slice(&name);
        boxed.extend_from_slice(&payload);
        let delta = boxed.len();
        let at = location.sample_entry_end;
        init.bytes.splice(at..at, boxed);
        // `ancestors` is hvcC first, then the sample entry and its parents.
        // The record goes beside hvcC rather than inside it, so hvcC itself
        // does not grow — everything from the sample entry up does.
        for &box_at in &location.ancestors[1..] {
            grow_box(&mut init.bytes, box_at, delta)?;
        }
        true
    };

    if changed {
        // `Init.tracks` is parsed metadata, and `dolby_vision_config` is
        // parsed from the box just written. Leaving it stale makes the in-
        // memory init disagree with its own bytes, and two live guards read it
        // as an inhibitor: HDR10 static-metadata promotion refuses to run on a
        // DV entry, and `sanitize_stale_dolby_brand` refuses to strip the
        // Dolby brand from one. Both would do the wrong thing on a stale
        // `false`, and both are on the path this writer exists to serve.
        let video_id = init.video().map(|track| track.id);
        if let Some(id) = video_id {
            if let Some(track) = init.tracks.iter_mut().find(|track| track.id == id) {
                track.dolby_vision_config = true;
            }
        }
    }
    Ok(changed)
}

/// Delete the Dolby Vision configuration record from the video sample entry.
///
/// Answers whether the init changed; an init that never carried one is not an
/// error.
///
/// This is the counterpart to [`set_dolby_vision_record`], and the two are not
/// interchangeable. A rewrite can say "Profile 8, no enhancement layer"; it
/// cannot say "this is not a Dolby Vision stream", because a record declaring
/// `rpu_present_flag = 0` still puts a `dvcC`/`dvvC` box in front of a decoder
/// that reads the box as the claim. Only removing it is honest about a stream
/// whose RPUs are gone.
///
/// The caller is the strip path on an ffmpeg without `dovi_rpu`. There, the
/// bitstream filter removes the RPU and enhancement-layer NAL units by type
/// while the DOVI *side data* ffmpeg copied out of the source survives, and
/// the muxer writes it out as a Profile 7 record with `el_present_flag = 1`
/// over a stream that has neither layer. Chrome ignores the box; VideoToolbox
/// honours it, and Safari answers with a software decode of 4K10 HEVC on
/// hardware that has a dedicated block for it — the 4K stutter
/// `docs/streaming/STUTTER-4K.md` exists to fix, reintroduced by the very filter meant
/// to prevent it.
///
/// The record is a sibling of `hvcC` inside the sample entry, not a child of
/// it, so `hvcC`'s own size is untouched and every box from the sample entry
/// up through `moov` shrinks by the whole box.
pub fn remove_dolby_vision_record(init: &mut Init) -> Result<bool, Fmp4Error> {
    // A loop, not a single removal. `locate_hvcc` reports the first of `dvvC`
    // and `dvcC` it finds and stops, so an entry carrying both — which no
    // muxer in the corpus writes, and which this function must therefore not
    // silently half-fix — would keep one box while the parsed
    // `dolby_vision_config` flag below said there were none, leaving the
    // in-memory init wrong about its own bytes with nothing to run again.
    let mut removed = false;
    while let Some(existing) = locate_hvcc(&init.bytes)?.and_then(|location| {
        location
            .dolby_vision
            .clone()
            .map(|record| (record, location.ancestors))
    }) {
        let (existing, ancestors) = existing;
        let box_start = existing
            .payload
            .start
            .checked_sub(existing.header_len)
            .ok_or_else(|| {
                Fmp4Error::Unsupported(
                    "the Dolby Vision record's header lies before the init".into(),
                )
            })?;
        let delta = existing.payload.end - box_start;

        // Sizes first, offsets second. Every ancestor's start lies before the
        // record, so its recorded offset is still valid here; after the
        // removal some would not be, and patching a size through a stale
        // offset writes four bytes into the middle of a box.
        //
        // `ancestors[0]` is `hvcC` and is deliberately skipped: the record
        // sits beside it, and shrinking `hvcC` would swallow whatever follows.
        for &box_at in ancestors.iter().skip(1) {
            shrink_box(&mut init.bytes, box_at, delta)?;
        }
        init.bytes.drain(box_start..existing.payload.end);
        removed = true;
    }
    if !removed {
        return Ok(false);
    }

    // `Init.tracks` is parsed metadata and `dolby_vision_config` was parsed
    // from the box just deleted. Two live guards read it and both would now do
    // the wrong thing on a stale `true`: HDR10 static-metadata promotion
    // refuses to run on a Dolby Vision entry, and `sanitize_stale_dolby_brand`
    // refuses to drop the `dby1` file-type brand from one — so the init would
    // keep advertising Dolby Vision in its `ftyp` with no record behind it,
    // which is the same class of lie one box further out.
    let video_id = init.video().map(|track| track.id);
    if let Some(id) = video_id {
        if let Some(track) = init.tracks.iter_mut().find(|track| track.id == id) {
            track.dolby_vision_config = false;
        }
    }

    // And the file-type brand, in the same call rather than in a second pass a
    // caller has to remember. `dby1` in `ftyp` beside a sample entry with no
    // record is the shape AVPlayer refuses outright — a contradictory
    // initialization segment — which is a worse failure than the software
    // decode this removal exists to prevent, and it is the failure this
    // function would otherwise create: the sanitizer that removes the brand
    // runs on the MUXER init, where the record is still present, so it
    // correctly declines, and nothing runs again after the record goes.
    //
    // Here rather than at the call site because the removal has to stay a pure
    // function of the stored promotion inputs: a brand fix applied anywhere
    // else would land on the live path and not on a head regeneration, and the
    // regeneration would then be refused for drift against an init it built
    // correctly.
    replace_dolby_file_type_brand(&mut init.bytes);
    Ok(true)
}

/// Rewrite the Dolby Vision `dby1` file-type brand to the plain ISO one.
///
/// Answers whether anything changed. Both the major brand and every entry in
/// the compatible-brands list, because a reader may consult either.
///
/// A `dby1` claim over a sample entry carrying no Dolby Vision record is not a
/// cosmetic leftover: AVPlayer treats the pair as a contradictory
/// initialization segment and fails the item immediately. Two callers reach
/// this — the copy segmenter, which sees an init ffmpeg's `dovi_rpu` filter
/// already emptied but whose brand movenc had written before the filter's
/// output parameters reached it, and [`remove_dolby_vision_record`], which
/// creates the same contradiction itself.
pub fn replace_dolby_file_type_brand(bytes: &mut [u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes(bytes[0..4].try_into().expect("four-byte ftyp size")) as usize;
    if size < 16 || size > bytes.len() || !(size - 16).is_multiple_of(4) {
        return false;
    }
    let mut changed = false;
    // The major brand at 8, then the compatible-brands list from 16 on. Bytes
    // 12..16 are the minor version, which is a number and not a brand.
    for offset in std::iter::once(8).chain((16..size).step_by(4)) {
        if &bytes[offset..offset + 4] == b"dby1" {
            bytes[offset..offset + 4].copy_from_slice(b"iso6");
            changed = true;
        }
    }
    changed
}

/// Whether the HEVC decoder configuration carries complete VPS, SPS, and PPS arrays.
///
/// The minimal legal hvcC record carries none. It is parseable, but it cannot
/// satisfy an `hvc1`/`dvh1` sample entry until promotion supplies all three,
/// each with its mandatory `array_completeness` flag.
pub fn hevc_parameter_sets_complete(init: &Init) -> Result<bool, Fmp4Error> {
    let Some(video) = init.video() else {
        return Ok(false);
    };
    if video.codec != Some(VideoCodec::Hevc) {
        return Ok(false);
    }
    let locations = locate_hevc_sample_entries(&init.bytes)?;
    if locations.is_empty() {
        return Ok(false);
    }
    for location in locations {
        let Some(location) = location else {
            return Ok(false);
        };
        if !hvcc_parameter_set_arrays_complete(&init.bytes[location.payload])? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Validate the decoder configuration promised by an HEVC sample entry.
///
/// `hev1`/`dvhe` may carry parameter sets in-band, so an empty or partial
/// hvcC remains a valid description for those entries. `hvc1`/`dvh1` make the
/// opposite promise: every required parameter set is out of band, and the
/// init is not publishable until hvcC contains VPS, SPS, and PPS arrays whose
/// `array_completeness` flags are all set. This ruling
/// comes from the emitted init itself rather than probe metadata, which may be
/// absent or stale.
pub fn validate_hevc_sample_entries(init: &Init) -> Result<HevcSampleEntryLayout, Fmp4Error> {
    let Some(video) = init.video() else {
        return Ok(HevcSampleEntryLayout::NotHevc);
    };
    if video.codec != Some(VideoCodec::Hevc) {
        return Ok(HevcSampleEntryLayout::NotHevc);
    }
    let locations = locate_hevc_sample_entries(&init.bytes)?;
    if locations.is_empty() {
        return Err(Fmp4Error::Unsupported(
            "the HEVC video sample entry has no hvcC box".into(),
        ));
    }
    let count = locations.len();
    for location in locations {
        let Some(location) = location else {
            return Err(Fmp4Error::Unsupported(
                "an HEVC video sample entry has no hvcC box".into(),
            ));
        };
        if location.parameter_sets_in_band {
            continue;
        }
        if !hvcc_parameter_set_arrays_complete(&init.bytes[location.payload])? {
            return Err(Fmp4Error::Unsupported(
                "an out-of-band HEVC decoder configuration has no complete VPS/SPS/PPS set".into(),
            ));
        }
    }
    if count == 1 {
        Ok(HevcSampleEntryLayout::Single)
    } else {
        Ok(HevcSampleEntryLayout::Multiple { count })
    }
}

/// Prove that the first video fragment selects one of the HEVC descriptions
/// validated above. The selection comes from `tfhd` when present and otherwise
/// from the track's `trex` default; both indexes are one-based.
pub fn validate_hevc_sample_description_reference(
    init: &Init,
    first: &Fragment,
) -> Result<(), Fmp4Error> {
    let Some(video) = init
        .video()
        .filter(|track| track.codec == Some(VideoCodec::Hevc))
    else {
        return Err(Fmp4Error::Unsupported(
            "the emitted init does not contain the promised HEVC video track".into(),
        ));
    };
    let description_count = locate_hevc_sample_entries(&init.bytes)?.len();
    let Some((moov_at, moov)) = find_child(&init.bytes, 0..init.bytes.len(), b"moov")? else {
        return malformed("init has no moov while reading trex");
    };
    let moov_body = moov_at.start + moov.header_len..moov_at.start + moov.size;
    let Some((mvex_at, mvex)) = find_child(&init.bytes, moov_body, b"mvex")? else {
        return malformed("fragmented init has no mvex");
    };
    let mvex_body = mvex_at.start + mvex.header_len..mvex_at.start + mvex.size;
    let mut default_index = None;
    for (trex_at, trex) in find_children(&init.bytes, mvex_body, b"trex")? {
        let payload = trex_at.start + trex.header_len..trex_at.start + trex.size;
        if payload.len() < 12 {
            return malformed("trex too short for sample-description index");
        }
        if be_u32(&init.bytes, payload.start + 4) == video.id {
            default_index = Some(be_u32(&init.bytes, payload.start + 8));
            break;
        }
    }
    let default_index = default_index.ok_or_else(|| {
        Fmp4Error::Malformed("video track has no trex sample-description default".into())
    })?;

    let moof = peek_box(&first.bytes, 0)?
        .filter(|header| header.kind() == b"moof")
        .ok_or_else(|| Fmp4Error::Malformed("first fragment has no moof".into()))?;
    let moof_body = moof.header_len..moof.size;
    let mut saw_video = false;
    for (traf_at, traf) in find_children(&first.bytes, moof_body, b"traf")? {
        let traf_body = traf_at.start + traf.header_len..traf_at.start + traf.size;
        let Some((tfhd_at, tfhd)) = find_child(&first.bytes, traf_body, b"tfhd")? else {
            return malformed("traf has no tfhd");
        };
        let payload = tfhd_at.start + tfhd.header_len..tfhd_at.start + tfhd.size;
        if payload.len() < 8 || be_u32(&first.bytes, payload.start + 4) != video.id {
            continue;
        }
        saw_video = true;
        let flags = be_u32(&first.bytes, payload.start) & 0x00ff_ffff;
        let index = if flags & 0x00_0002 != 0 {
            let index_offset = 8 + if flags & 1 != 0 { 8 } else { 0 };
            if payload.len() < index_offset + 4 {
                return malformed("tfhd truncated at sample_description_index");
            }
            be_u32(&first.bytes, payload.start + index_offset)
        } else {
            default_index
        };
        if index == 0 || index as usize > description_count {
            return Err(Fmp4Error::Unsupported(format!(
                "video fragment selects HEVC sample description {index}, but init declares {description_count}"
            )));
        }
    }
    if !saw_video {
        return Err(Fmp4Error::Unsupported(
            "the first media fragment contains no promised HEVC video track".into(),
        ));
    }
    Ok(())
}

/// Exact Main10/PQ/BT.2020/limited-range output configuration read from the
/// private encoded artifact. Catalog tags and encoder argv cannot substitute
/// for these bytes. SPS header probing remains the daemon's independent check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcHdr10SampleEntryFacts {
    pub width: u16,
    pub height: u16,
    pub profile_idc: u8,
    pub luma_bit_depth: u8,
    pub chroma_bit_depth: u8,
}

pub fn hevc_hdr10_sample_entry_facts(init: &Init) -> Result<HevcHdr10SampleEntryFacts, Fmp4Error> {
    hevc_processed_sample_entry_facts(init, false)
}

/// Processed SDR uses BT.709 signalling (1/1/1); the pixel EOTF is gamma 2.4.
/// There is no distinct H.273 gamma-2.4 transfer code.
pub fn hevc_processed_sample_entry_facts(
    init: &Init,
    sdr: bool,
) -> Result<HevcHdr10SampleEntryFacts, Fmp4Error> {
    validate_hevc_decoder_configuration(init)?;
    let video = init
        .video()
        .ok_or_else(|| Fmp4Error::Unsupported("no encoded video".into()))?;
    if video.codec != Some(VideoCodec::Hevc) || video.has_edit_list {
        return Err(Fmp4Error::Unsupported(
            "encoded HDR10 requires HEVC without edits".into(),
        ));
    }
    if validate_hevc_sample_entries(init)? != HevcSampleEntryLayout::Single {
        return Err(Fmp4Error::Unsupported(
            "encoded HDR10 needs one HEVC description".into(),
        ));
    }
    let location = locate_configuring_hvcc(&init.bytes)?
        .ok_or_else(|| Fmp4Error::Malformed("missing encoded hvcC".into()))?;
    let configuration = &init.bytes[location.payload.clone()];
    let profile_idc = configuration[1] & 0x1f;
    let luma_bit_depth = 8 + (configuration[17] & 7);
    let chroma_bit_depth = 8 + (configuration[18] & 7);
    if configuration[21] & 3 != 3
        || profile_idc != 2
        || configuration[16] & 3 != 1
        || luma_bit_depth != 10
        || chroma_bit_depth != 10
    {
        return Err(Fmp4Error::Unsupported(
            "encoded HEVC is not Main10 YUV420P10".into(),
        ));
    }
    let entry = location.ancestors[1];
    let body = entry.start + entry.header_len;
    let width = u16::from_be_bytes(
        init.bytes[body + 24..body + 26]
            .try_into()
            .expect("validated visual header"),
    );
    let height = u16::from_be_bytes(
        init.bytes[body + 26..body + 28]
            .try_into()
            .expect("validated visual header"),
    );
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return malformed("encoded HDR10 has invalid raster");
    }
    let colors = find_children(&init.bytes, body + 78..location.sample_entry_end, b"colr")?;
    if colors.len() != 1 {
        return Err(Fmp4Error::Unsupported(
            "encoded HDR10 needs one color record".into(),
        ));
    }
    let (at, color) = &colors[0];
    let color = &init.bytes[at.start + color.header_len..at.start + color.size];
    let expected = if sdr { [1, 1, 1] } else { [9, 16, 9] };
    if color.len() != 11
        || color[..4] != *b"nclx"
        || u16::from_be_bytes([color[4], color[5]]) != expected[0]
        || u16::from_be_bytes([color[6], color[7]]) != expected[1]
        || u16::from_be_bytes([color[8], color[9]]) != expected[2]
        || color[10] != 0
    {
        return Err(Fmp4Error::Unsupported(
            "encoded color is not limited BT.2020 PQ NCL".into(),
        ));
    }
    Ok(HevcHdr10SampleEntryFacts {
        width,
        height,
        profile_idc,
        luma_bit_depth,
        chroma_bit_depth,
    })
}

pub fn validate_hevc_decoder_configuration(init: &Init) -> Result<(), Fmp4Error> {
    validate_hevc_sample_entries(init).map(|_| ())
}

/// How one fragment's in-band HEVC parameter sets relate to its sample entry.
///
/// The measurement behind `transcode::hevc_census`: whether deleting the
/// in-band sets, as every historical copy does, loses a definition the
/// decoder needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InBandParameterSets {
    /// No video sample carried a parameter set of a type the sample entry
    /// configures.
    Absent,
    /// Every one carried repeats a definition the sample entry holds.
    MatchSampleEntry,
    /// At least one redefines what the sample entry says.
    DifferFromSampleEntry,
}

/// Compare every in-band VPS/SPS/PPS in `fragment`'s video samples with the
/// definitions in the sample entry's `hvcC`.
///
/// Byte comparison of the NAL units, ignoring trailing zero bytes (which a
/// muxer may pad or trim without changing a definition). A type the record
/// does not configure is not compared: filling a missing array is what
/// promotion is for, and that path already keeps what it needs.
pub fn compare_in_band_parameter_sets(
    init: &Init,
    fragment: &Fragment,
) -> Result<InBandParameterSets, Fmp4Error> {
    let Some(video) = init.video() else {
        return Ok(InBandParameterSets::Absent);
    };
    if video.codec != Some(VideoCodec::Hevc) || video.nal_length_size == 0 {
        return Ok(InBandParameterSets::Absent);
    }
    let Some(location) = locate_configuring_hvcc(&init.bytes)? else {
        return Ok(InBandParameterSets::Absent);
    };
    let configured = hvcc_parameter_set_nals(&init.bytes[location.payload])?;
    let Some(track) = fragment.track(video.id) else {
        return Ok(InBandParameterSets::Absent);
    };
    let mut seen = false;
    for run in &track.runs {
        let mut start = run.data_offset;
        for sample in &run.samples {
            let end = start
                .checked_add(sample.size as usize)
                .filter(|end| *end <= fragment.bytes.len())
                .ok_or_else(|| {
                    Fmp4Error::Malformed("a sample points past the end of its fragment".into())
                })?;
            for nal in hevc_parameter_set_nals(&fragment.bytes[start..end], video.nal_length_size) {
                let kind = hevc_nal_type(nal);
                if !configured.iter().any(|known| hevc_nal_type(known) == kind) {
                    continue;
                }
                seen = true;
                let nal = without_trailing_zero_bytes(nal);
                if !configured
                    .iter()
                    .any(|known| without_trailing_zero_bytes(known) == nal)
                {
                    return Ok(InBandParameterSets::DifferFromSampleEntry);
                }
            }
            start = end;
        }
    }
    Ok(if seen {
        InBandParameterSets::MatchSampleEntry
    } else {
        InBandParameterSets::Absent
    })
}

fn without_trailing_zero_bytes(nal: &[u8]) -> &[u8] {
    let end = nal
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |at| at + 1);
    &nal[..end]
}

/// The VPS/SPS/PPS NAL units an `hvcC` record carries, in record order.
fn hvcc_parameter_set_nals(record: &[u8]) -> Result<Vec<&[u8]>, Fmp4Error> {
    if record.len() < 23 {
        return malformed("hvcC too short for its NAL arrays");
    }
    let mut found = Vec::new();
    let mut pos = 23usize;
    for _ in 0..record[22] {
        if pos + 3 > record.len() {
            return malformed("hvcC NAL array header runs past the record");
        }
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]) as usize;
        pos += 3;
        for _ in 0..count {
            if pos + 2 > record.len() {
                return malformed("hvcC NAL length runs past the record");
            }
            let len = u16::from_be_bytes([record[pos], record[pos + 1]]) as usize;
            pos += 2;
            let end = pos
                .checked_add(len)
                .filter(|end| *end <= record.len())
                .ok_or_else(|| Fmp4Error::Malformed("hvcC NAL runs past the record".into()))?;
            let nal = &record[pos..end];
            if matches!(hevc_nal_type(nal), Some(32..=34)) {
                found.push(nal);
            }
            pos = end;
        }
    }
    Ok(found)
}

fn hevc_parameter_set_nals(sample: &[u8], length_size: u8) -> Vec<&[u8]> {
    length_prefixed_nals(sample, length_size)
        .into_iter()
        .filter(|nal| matches!(hevc_nal_type(nal), Some(32..=34)))
        .collect()
}

fn hevc_nal_type(nal: &[u8]) -> Option<u8> {
    (nal.len() >= 2).then(|| (nal[0] >> 1) & 0x3f)
}

fn length_prefixed_nals(sample: &[u8], length_size: u8) -> Vec<&[u8]> {
    let lsz = length_size as usize;
    let mut out = Vec::new();
    let mut pos = 0usize;
    while lsz > 0 && pos.checked_add(lsz).is_some_and(|end| end <= sample.len()) {
        let mut len = 0usize;
        for byte in &sample[pos..pos + lsz] {
            len = (len << 8) | *byte as usize;
        }
        let Some(start) = pos.checked_add(lsz) else {
            break;
        };
        let Some(end) = start.checked_add(len) else {
            break;
        };
        let Some(nal) = sample.get(start..end) else {
            break;
        };
        out.push(nal);
        if len == 0 {
            break;
        }
        pos = end;
    }
    out
}

/// Put HDR10's static metadata where an HLS decoder can see it before opening
/// the first media fragment.
///
/// ffmpeg's fragmented-MP4 copy path can carry mastering-display (SEI payload
/// type 137) and content-light-level (144) messages only in the first video
/// sample. That is enough for an elementary-stream decoder, but not for Apple
/// HLS: AVPlayer decides whether a `VIDEO-RANGE=PQ` variant is supported from
/// its initialization segment. Copy the original prefix-SEI NAL units into
/// the `hvcC` record, exactly as authored, and add Apple's recommended
/// `mdcv`/`clli` sample-entry boxes while leaving every media sample untouched.
///
/// Returns `true` when the init segment changed. Non-HEVC streams, streams
/// without the metadata, real Dolby Vision sample entries, and already-rich
/// `hvcC` records are no-ops.
pub fn promote_hdr10_static_metadata(init: &mut Init, first: &Fragment) -> Result<bool, Fmp4Error> {
    let inputs = PromotionInputs::from_fragment(first, init);
    promote_hdr10_static_metadata_from(init, &inputs.hdr10_sei)
}

/// The same surgery, from prefix SEI captured somewhere else.
pub fn promote_hdr10_static_metadata_from(
    init: &mut Init,
    hdr10_sei: &[Vec<u8>],
) -> Result<bool, Fmp4Error> {
    let Some(video) = init.video() else {
        return Ok(false);
    };
    if video.codec != Some(VideoCodec::Hevc)
        || video.dolby_vision_config
        || video.nal_length_size == 0
    {
        return Ok(false);
    }

    let candidate_nals: Vec<&[u8]> = hdr10_sei.iter().map(Vec::as_slice).collect();
    if candidate_nals.is_empty() {
        return Ok(false);
    }

    let Some(location) = locate_hvcc(&init.bytes)? else {
        return Err(Fmp4Error::Unsupported(
            "the HEVC video sample entry has no hvcC box to enrich".into(),
        ));
    };
    let record = &init.bytes[location.payload.clone()];
    let present = hvcc_hdr10_sei_types(record)?;
    let mut additions = Vec::new();
    let mut covered = present;
    let mut mastering = None;
    let mut content_light = None;
    for &nal in &candidate_nals {
        for (kind, payload) in hdr10_sei_messages(nal) {
            match kind {
                137 if mastering.is_none() => mastering = Some(payload),
                144 if content_light.is_none() => content_light = Some(payload),
                _ => {}
            }
        }
        let types = hdr10_sei_types(nal);
        if types.iter().all(|kind| covered.contains(kind)) {
            continue;
        }
        for kind in types {
            if !covered.contains(&kind) {
                covered.push(kind);
            }
        }
        additions.push(nal);
    }
    if additions.len() > u16::MAX as usize {
        return Err(Fmp4Error::Unsupported(
            "too many HDR10 prefix-SEI NAL units for hvcC".into(),
        ));
    }

    let mut changed = false;
    let mut hvcc_delta = 0usize;
    if !additions.is_empty() {
        let mut array = Vec::new();
        // array_completeness=0: these are the decoder-wide static records,
        // not a claim that no other per-picture prefix SEIs occur in samples.
        array.push(39);
        array.extend_from_slice(&(additions.len() as u16).to_be_bytes());
        for nal in additions {
            let len = u16::try_from(nal.len()).map_err(|_| {
                Fmp4Error::Unsupported("an HDR10 prefix-SEI NAL exceeds hvcC's u16 length".into())
            })?;
            array.extend_from_slice(&len.to_be_bytes());
            array.extend_from_slice(nal);
        }

        let arrays_offset = location.payload.start + 22;
        let arrays = init.bytes[arrays_offset];
        if arrays == u8::MAX {
            return Err(Fmp4Error::Unsupported(
                "hvcC already declares 255 NAL arrays".into(),
            ));
        }
        hvcc_delta = array.len();
        let insert_at = location.payload.end;
        init.bytes.splice(insert_at..insert_at, array);
        init.bytes[arrays_offset] = arrays + 1;
        for &box_at in &location.ancestors {
            grow_box(&mut init.bytes, box_at, hvcc_delta)?;
        }
        changed = true;
    }

    // Apple recommends explicit sample-entry atoms even though the same SEIs
    // in hvcC are a valid fallback. Carry both forms: older AVFoundation
    // versions have been observed consulting the atoms while deciding whether
    // a PQ HLS variant is supported, before they instantiate the decoder.
    let mut static_boxes = Vec::new();
    if !location.has_mdcv {
        if let Some(payload) = mastering.filter(|payload| payload.len() == 24) {
            static_boxes.extend_from_slice(&mp4_box(b"mdcv", &payload)?);
        }
    }
    if !location.has_clli {
        if let Some(payload) = content_light.filter(|payload| payload.len() == 4) {
            static_boxes.extend_from_slice(&mp4_box(b"clli", &payload)?);
        }
    }
    if !static_boxes.is_empty() {
        let delta = static_boxes.len();
        let insert_at = location.sample_entry_end + hvcc_delta;
        init.bytes.splice(insert_at..insert_at, static_boxes);
        // Skip hvcC itself; grow the visual sample entry and every container.
        for &box_at in &location.ancestors[1..] {
            grow_box(&mut init.bytes, box_at, delta)?;
        }
        changed = true;
    }
    Ok(changed)
}

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>, Fmp4Error> {
    let size = payload
        .len()
        .checked_add(8)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| Fmp4Error::Unsupported("HDR10 metadata box exceeds u32".into()))?;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

fn first_video_sample<'a>(fragment: &'a Fragment, video: &Track) -> Option<&'a [u8]> {
    let track = fragment.track(video.id)?;
    let run = track.runs.first()?;
    let sample = run.samples.first()?;
    let end = run.data_offset.checked_add(sample.size as usize)?;
    fragment.bytes.get(run.data_offset..end)
}

/// Prefix-SEI NALs from one length-prefixed HEVC sample that carry at least
/// one HDR10 static-metadata message.
fn hdr10_prefix_sei_nals(sample: &[u8], length_size: u8) -> Vec<&[u8]> {
    length_prefixed_nals(sample, length_size)
        .into_iter()
        .filter(|nal| hevc_nal_type(nal) == Some(39) && !hdr10_sei_types(nal).is_empty())
        .collect()
}

/// HDR10 static-metadata payload types carried by one HEVC SEI NAL.
fn hdr10_sei_types(nal: &[u8]) -> Vec<u16> {
    hdr10_sei_messages(nal)
        .into_iter()
        .map(|(kind, _)| kind)
        .collect()
}

/// HDR10 messages and their unescaped big-endian payloads from one HEVC SEI
/// NAL. The payload bytes are exactly what the `mdcv`/`clli` boxes carry.
fn hdr10_sei_messages(nal: &[u8]) -> Vec<(u16, Vec<u8>)> {
    if nal.len() < 3 {
        return Vec::new();
    }
    // Remove emulation-prevention bytes before reading SEI payload headers.
    let mut rbsp = Vec::with_capacity(nal.len() - 2);
    let mut zeroes = 0u8;
    for &byte in &nal[2..] {
        if zeroes >= 2 && byte == 3 {
            continue;
        }
        rbsp.push(byte);
        zeroes = if byte == 0 {
            zeroes.saturating_add(1)
        } else {
            0
        };
    }

    let mut found = Vec::new();
    let mut pos = 0usize;
    while pos < rbsp.len() {
        // rbsp_trailing_bits, possibly followed by zero padding.
        if rbsp[pos] == 0x80 && rbsp[pos + 1..].iter().all(|byte| *byte == 0) {
            break;
        }
        let Some(kind) = sei_number(&rbsp, &mut pos) else {
            break;
        };
        let Some(size) = sei_number(&rbsp, &mut pos) else {
            break;
        };
        let Some(end) = pos.checked_add(size as usize) else {
            break;
        };
        if end > rbsp.len() {
            break;
        }
        if matches!(kind, 137 | 144) && !found.iter().any(|(seen, _)| *seen == kind) {
            found.push((kind, rbsp[pos..end].to_vec()));
        }
        pos = end;
    }
    found
}

fn sei_number(bytes: &[u8], pos: &mut usize) -> Option<u16> {
    let mut value = 0u16;
    loop {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        value = value.checked_add(byte as u16)?;
        if byte != 0xff {
            return Some(value);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BoxAt {
    start: usize,
    header_len: usize,
}

/// Where a Dolby Vision configuration record sits, and how it is framed.
///
/// The header length is carried rather than assumed to be 8: a `largesize`
/// box puts a 64-bit length where an ordinary box puts its name, and writing
/// the name over it produces an init that no longer parses — silently, since
/// the write itself succeeds.
#[derive(Clone)]
struct DolbyVisionAt {
    /// `dvcC` for profiles up to 7, `dvvC` for 8 and above. Which one a muxer
    /// wrote is a fact about the muxer, so it is read rather than assumed.
    name: [u8; 4],
    header_len: usize,
    payload: Range<usize>,
}

struct HvcCLocation {
    payload: Range<usize>,
    sample_entry_end: usize,
    /// `hev1`/`dvhe` permit parameter sets in samples; `hvc1`/`dvh1` promise
    /// that the decoder configuration is carried out of band in hvcC.
    parameter_sets_in_band: bool,
    has_mdcv: bool,
    has_clli: bool,
    /// The Dolby Vision configuration record already in this sample entry, if
    /// there is one: its four-character name and its payload range.
    ///
    /// Two names for one 24-byte record — `dvcC` is the profile ≤ 7 spelling
    /// and `dvvC` the profile ≥ 8 one. Which one is present is a fact about
    /// the muxer that wrote it, so both are found and the name is carried
    /// rather than assumed.
    dolby_vision: Option<DolbyVisionAt>,
    /// hvcC first, then every enclosing box through moov.
    ancestors: Vec<BoxAt>,
}

#[derive(Clone)]
struct AvcCLocation {
    sample_entry: [u8; 4],
    payload: Range<usize>,
    /// Structural visual header used by actual raster/color facts and fixtures.
    entry: BoxAt,
    /// `stsd` first, then every enclosing box through `moov`. Fixture-only,
    /// like `entry`.
    #[cfg_attr(not(any(test, feature = "fixtures")), expect(dead_code))]
    ancestors: Vec<BoxAt>,
}

fn track_id_in_trak(bytes: &[u8], trak_body: Range<usize>) -> Result<Option<u32>, Fmp4Error> {
    let Some((tkhd_at, tkhd)) = find_child(bytes, trak_body, b"tkhd")? else {
        return Ok(None);
    };
    let body = tkhd_at.start + tkhd.header_len..tkhd_at.start + tkhd.size;
    let version = *bytes
        .get(body.start)
        .ok_or_else(|| Fmp4Error::Malformed("tkhd too short while locating stsd".into()))?;
    let id_at = body.start + if version == 1 { 20 } else { 12 };
    if id_at + 4 > body.end {
        return malformed("tkhd too short while locating stsd");
    }
    Ok(Some(be_u32(bytes, id_at)))
}

/// Locate AVC configurations structurally under the selected track's
/// `trak/mdia/minf/stbl/stsd/avc1|avc3|dva1|dvav` chain.
fn locate_avc_sample_entries(
    bytes: &[u8],
    track_id: u32,
) -> Result<(usize, Vec<AvcCLocation>), Fmp4Error> {
    let mut result = None;
    let Some((moov_at, moov)) = find_child(bytes, 0..bytes.len(), b"moov")? else {
        return Ok((0, Vec::new()));
    };
    let moov_body = moov_at.start + moov.header_len..moov_at.start + moov.size;
    for (trak_at, trak) in find_children(bytes, moov_body, b"trak")? {
        let trak_body = trak_at.start + trak.header_len..trak_at.start + trak.size;
        if track_id_in_trak(bytes, trak_body.clone())? != Some(track_id) {
            continue;
        }
        if result.is_some() {
            return malformed("more than one trak carries the selected video track id");
        }
        let Some((mdia_at, mdia)) = find_child(bytes, trak_body, b"mdia")? else {
            return malformed("selected video trak has no mdia");
        };
        let mdia_body = mdia_at.start + mdia.header_len..mdia_at.start + mdia.size;
        let Some((minf_at, minf)) = find_child(bytes, mdia_body, b"minf")? else {
            return malformed("selected video mdia has no minf");
        };
        let minf_body = minf_at.start + minf.header_len..minf_at.start + minf.size;
        let Some((stbl_at, stbl)) = find_child(bytes, minf_body, b"stbl")? else {
            return malformed("selected video minf has no stbl");
        };
        let stbl_body = stbl_at.start + stbl.header_len..stbl_at.start + stbl.size;
        let Some((stsd_at, stsd)) = find_child(bytes, stbl_body, b"stsd")? else {
            return malformed("selected video stbl has no stsd");
        };
        let stsd_body = stsd_at.start + stsd.header_len..stsd_at.start + stsd.size;
        if stsd_body.start + 8 > stsd_body.end {
            return malformed("stsd too short while locating avcC");
        }
        let declared = be_u32(bytes, stsd_body.start + 4) as usize;
        let entries_start = stsd_body.start + 8;
        let entries = children(&bytes[entries_start..stsd_body.end])?;
        if entries.len() != declared {
            return malformed("stsd entry_count does not match its sample entries");
        }
        let mut locations = Vec::new();
        for (entry, payload_start, entry_end) in entries {
            if !matches!(entry.kind(), b"avc1" | b"avc3" | b"dva1" | b"dvav") {
                continue;
            }
            let entry_at = BoxAt {
                start: entries_start + payload_start - entry.header_len,
                header_len: entry.header_len,
            };
            let body_start = entries_start + payload_start;
            let body_end = entries_start + entry_end;
            let extra_start = body_start + 78;
            if extra_start > body_end {
                return malformed("visual sample entry too short while locating avcC");
            }
            let Some((avcc_at, avcc)) = find_child(bytes, extra_start..body_end, b"avcC")? else {
                continue;
            };
            let payload = avcc_at.start + avcc.header_len..avcc_at.start + avcc.size;
            if payload.len() < 5 {
                return malformed("avcC too short");
            }
            locations.push(AvcCLocation {
                sample_entry: *entry.kind(),
                payload,
                entry: entry_at,
                ancestors: vec![stsd_at, stbl_at, minf_at, mdia_at, trak_at, moov_at],
            });
        }
        result = Some((declared, locations));
    }
    result.ok_or_else(|| Fmp4Error::Malformed("selected video track has no trak".into()))
}

fn locate_hevc_sample_entries(bytes: &[u8]) -> Result<Vec<Option<HvcCLocation>>, Fmp4Error> {
    let mut locations = Vec::new();
    let Some((moov_at, moov)) = find_child(bytes, 0..bytes.len(), b"moov")? else {
        return Ok(locations);
    };
    let moov_body = moov_at.start + moov.header_len..moov_at.start + moov.size;
    for (trak_at, trak) in find_children(bytes, moov_body, b"trak")? {
        let trak_body = trak_at.start + trak.header_len..trak_at.start + trak.size;
        let Some((mdia_at, mdia)) = find_child(bytes, trak_body, b"mdia")? else {
            continue;
        };
        let mdia_body = mdia_at.start + mdia.header_len..mdia_at.start + mdia.size;
        let Some((minf_at, minf)) = find_child(bytes, mdia_body, b"minf")? else {
            continue;
        };
        let minf_body = minf_at.start + minf.header_len..minf_at.start + minf.size;
        let Some((stbl_at, stbl)) = find_child(bytes, minf_body, b"stbl")? else {
            continue;
        };
        let stbl_body = stbl_at.start + stbl.header_len..stbl_at.start + stbl.size;
        let Some((stsd_at, stsd)) = find_child(bytes, stbl_body, b"stsd")? else {
            continue;
        };
        let entries_start = stsd_at.start + stsd.header_len + 8;
        let entries_end = stsd_at.start + stsd.size;
        if entries_start > entries_end {
            return malformed("stsd too short while locating hvcC");
        }
        // Keep every HEVC description. Mutators deliberately reject more than
        // one below; validation must inspect every description before the
        // multi-entry shape may authorize a non-mutating fallback.
        let hevc_entries: Vec<_> = find_children(bytes, entries_start..entries_end, b"hvc1")?
            .into_iter()
            .chain(find_children(bytes, entries_start..entries_end, b"hev1")?)
            .chain(find_children(bytes, entries_start..entries_end, b"dvh1")?)
            .chain(find_children(bytes, entries_start..entries_end, b"dvhe")?)
            .collect();
        for (entry_at, entry) in hevc_entries {
            let extra_start = entry_at.start + entry.header_len + 78;
            let extra_end = entry_at.start + entry.size;
            if extra_start > extra_end {
                return malformed("visual sample entry too short while locating hvcC");
            }
            let Some((hvcc_at, hvcc)) = find_child(bytes, extra_start..extra_end, b"hvcC")? else {
                locations.push(None);
                continue;
            };
            let payload = hvcc_at.start + hvcc.header_len..hvcc_at.start + hvcc.size;
            if payload.len() < 23 {
                return malformed("hvcC too short for its NAL arrays");
            }
            locations.push(Some(HvcCLocation {
                payload,
                sample_entry_end: extra_end,
                parameter_sets_in_band: matches!(entry.kind(), b"hev1" | b"dvhe"),
                has_mdcv: find_child(bytes, extra_start..extra_end, b"mdcv")?.is_some(),
                has_clli: find_child(bytes, extra_start..extra_end, b"clli")?.is_some(),
                dolby_vision: {
                    let mut found = None;
                    for name in [b"dvvC", b"dvcC"] {
                        if let Some((at, hdr)) = find_child(bytes, extra_start..extra_end, name)? {
                            found = Some(DolbyVisionAt {
                                name: *name,
                                header_len: hdr.header_len,
                                payload: at.start + hdr.header_len..at.start + hdr.size,
                            });
                            break;
                        }
                    }
                    found
                },
                ancestors: vec![
                    hvcc_at, entry_at, stsd_at, stbl_at, minf_at, mdia_at, trak_at, moov_at,
                ],
            }));
        }
    }
    Ok(locations)
}

/// Remove HEVC sample descriptions that repeat the first one's decoder
/// configuration, returning how many were removed.
///
/// FFmpeg 8's MOV muxer appends a sample description whenever a packet carries
/// new-extradata side data whose bytes differ from the current description.
/// `extract_extradata` — which the copy path needs for Matroska sources whose
/// `hvcC` is the bare 23-byte header — attaches that side data to every
/// keyframe carrying in-band parameter sets, and one keyframe's extraction
/// differs from another's only in its SEI arrays or a PPS trailing zero byte.
/// The result is an `stsd` declaring two descriptions that configure the
/// decoder identically, with every fragment selecting the first (the muxer
/// writes no `tfhd` sample-description index; `trex` defaults to 1). Measured
/// 2026-10-04 on six 2160p sources under jellyfin-ffmpeg 8.1.3: all six
/// produced exactly this shape, so every such title's live-HLS copy and every
/// fragment-index build refused it.
///
/// Removal is lossless only when the descriptions really are one decoder
/// configuration, so the proof is strict: one `stsd`, every entry the same
/// sample-entry fourcc, every byte of each entry outside its `hvcC` identical
/// (visual fields, `colr`, `pasp`, any Dolby Vision record), the same 22-byte
/// `hvcC` fixed header, and the same set of VPS/SPS/PPS units once trailing
/// zero bytes are disregarded, with the track's `trex` selecting the first
/// description. SEI arrays may differ. Anything else is left untouched and the mutators
/// keep refusing it as [`Fmp4Error::MultipleHevcSampleEntries`].
///
/// The description kept is rewritten to its parameter sets alone: VPS, SPS and
/// PPS arrays in record order, trailing zero bytes trimmed, duplicates and SEI
/// arrays dropped. FFmpeg 8's extraction copies whatever SEI the generation's
/// first keyframe carried — a decoded-picture hash, a per-scene message — so
/// two generations of one film started at different keyframes would otherwise
/// describe themselves differently, and VOD's init identity (one muxer init
/// per rendition across every generation) would refuse every later one. The
/// samples keep their in-band SEI, and HDR10 static metadata is promoted from
/// them as before.
pub fn collapse_equivalent_hevc_sample_entries(init: &mut Init) -> Result<usize, Fmp4Error> {
    // Like every other init mutator here: no parsed HEVC track, nothing to do.
    if !init
        .video()
        .is_some_and(|video| video.codec == Some(VideoCodec::Hevc))
    {
        return Ok(0);
    }
    let Some(entries) = equivalent_hevc_sample_entries(&init.bytes)? else {
        return Ok(0);
    };
    let removed = entries.len() - 1;
    let first = &entries[0];
    let stsd = first.ancestors[2];
    let start = first.sample_entry_end;
    let end = entries
        .last()
        .expect("at least two equivalent entries")
        .sample_entry_end;
    let delta = end - start;
    init.bytes.drain(start..end);
    let entry_count_at = stsd.start + stsd.header_len + 4;
    write_be_u32(&mut init.bytes, entry_count_at, 1)?;
    for &ancestor in &first.ancestors[2..] {
        shrink_box(&mut init.bytes, ancestor, delta)?;
    }
    // The kept entry precedes the drained span, so its offsets still hold.
    let payload = first.payload.clone();
    let canonical = parameter_set_only_hvcc(&init.bytes[payload.clone()])?;
    let old_len = payload.len();
    let new_len = canonical.len();
    init.bytes.splice(payload, canonical);
    if new_len < old_len {
        for &ancestor in &first.ancestors {
            shrink_box(&mut init.bytes, ancestor, old_len - new_len)?;
        }
    } else if new_len > old_len {
        for &ancestor in &first.ancestors {
            grow_box(&mut init.bytes, ancestor, new_len - old_len)?;
        }
    }
    Ok(removed)
}

/// An `hvcC` record reduced to its parameter sets: the fixed header, then the
/// VPS/SPS/PPS arrays in record order with each array's completeness byte
/// kept, every NAL's trailing zero bytes trimmed and repeats dropped.
fn parameter_set_only_hvcc(record: &[u8]) -> Result<Vec<u8>, Fmp4Error> {
    if record.len() < 23 {
        return malformed("hvcC too short for its NAL arrays");
    }
    let mut out = record[..22].to_vec();
    out.push(0);
    let mut arrays = 0u8;
    let mut pos = 23usize;
    for _ in 0..record[22] {
        if pos + 3 > record.len() {
            return malformed("hvcC NAL array header runs past the record");
        }
        let head = record[pos];
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]) as usize;
        pos += 3;
        let mut nals: Vec<&[u8]> = Vec::new();
        for _ in 0..count {
            if pos + 2 > record.len() {
                return malformed("hvcC NAL length runs past the record");
            }
            let len = u16::from_be_bytes([record[pos], record[pos + 1]]) as usize;
            pos += 2;
            let end = pos
                .checked_add(len)
                .filter(|end| *end <= record.len())
                .ok_or_else(|| Fmp4Error::Malformed("hvcC NAL runs past the record".into()))?;
            let nal = without_trailing_zero_bytes(&record[pos..end]);
            pos = end;
            if !nal.is_empty() && !nals.contains(&nal) {
                nals.push(nal);
            }
        }
        if !matches!(head & 0x3f, 32..=34) || nals.is_empty() {
            continue;
        }
        out.push(head);
        out.extend_from_slice(&(nals.len() as u16).to_be_bytes());
        for nal in nals {
            out.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            out.extend_from_slice(nal);
        }
        arrays += 1;
    }
    out[22] = arrays;
    Ok(out)
}

/// Every HEVC description of the init when there are several and they are one
/// decoder configuration (see [`collapse_equivalent_hevc_sample_entries`]), in
/// `stsd` order; `None` for a single description or any other shape.
fn equivalent_hevc_sample_entries(bytes: &[u8]) -> Result<Option<Vec<HvcCLocation>>, Fmp4Error> {
    let locations = locate_hevc_sample_entries(bytes)?;
    if locations.len() < 2 {
        return Ok(None);
    }
    let mut entries = Vec::with_capacity(locations.len());
    for location in locations {
        let Some(location) = location else {
            return Ok(None);
        };
        entries.push(location);
    }
    let stsd = entries[0].ancestors[2];
    if entries
        .iter()
        .any(|entry| entry.ancestors[2].start != stsd.start)
    {
        return Ok(None);
    }
    // A non-HEVC description beside them (or a count the walk disagrees with)
    // is not a repeated configuration.
    let declared = be_u32(bytes, stsd.start + stsd.header_len + 4) as usize;
    if declared != entries.len() {
        return Ok(None);
    }
    struct Shape<'a> {
        kind: [u8; 4],
        before: &'a [u8],
        after: &'a [u8],
        header: &'a [u8],
        parameter_sets: Vec<&'a [u8]>,
    }
    fn shape<'a>(bytes: &'a [u8], entry: &HvcCLocation) -> Result<Shape<'a>, Fmp4Error> {
        let sample_entry = entry.ancestors[1];
        let hvcc = entry.ancestors[0];
        let header = peek_box(bytes, sample_entry.start)?
            .ok_or_else(|| Fmp4Error::Malformed("sample entry truncated".into()))?;
        let record = &bytes[entry.payload.clone()];
        let mut parameter_sets: Vec<&[u8]> = hvcc_parameter_set_nals(record)?
            .into_iter()
            .map(without_trailing_zero_bytes)
            .collect();
        parameter_sets.sort_unstable();
        parameter_sets.dedup();
        Ok(Shape {
            kind: *header.kind(),
            before: &bytes[sample_entry.start + sample_entry.header_len..hvcc.start],
            after: &bytes[entry.payload.end..entry.sample_entry_end],
            header: &record[..22],
            parameter_sets,
        })
    }
    // A description this cannot read is not provably equivalent; leave the
    // refusal (and its classification) to the validators that own it.
    let Ok(first) = shape(bytes, &entries[0]) else {
        return Ok(None);
    };
    if first.parameter_sets.is_empty() {
        // Nothing to prove equality with; the incomplete-configuration
        // refusal belongs to validation, not to this collapse.
        return Ok(None);
    }
    for entry in &entries[1..] {
        let Ok(other) = shape(bytes, entry) else {
            return Ok(None);
        };
        if other.kind != first.kind
            || other.before != first.before
            || other.after != first.after
            || other.header != first.header
            || other.parameter_sets != first.parameter_sets
        {
            return Ok(None);
        }
    }
    // Contiguity: the removal drains one span after the first entry.
    for pair in entries.windows(2) {
        if pair[1].ancestors[1].start != pair[0].sample_entry_end {
            return Ok(None);
        }
    }
    // Fragments that rely on `trex` must already select the description kept;
    // equivalence does not make a dangling index valid.
    if trex_default_description(bytes, &entries[0])? != Some(1) {
        return Ok(None);
    }
    Ok(Some(entries))
}

/// The `trex` default sample-description index of the track an HEVC entry
/// belongs to, or `None` when the init declares no `trex` for it.
fn trex_default_description(bytes: &[u8], entry: &HvcCLocation) -> Result<Option<u32>, Fmp4Error> {
    let trak = entry.ancestors[6];
    let moov = entry.ancestors[7];
    let Some(trak_header) = peek_box(bytes, trak.start)? else {
        return Ok(None);
    };
    let Some(track_id) = track_id_in_trak(
        bytes,
        trak.start + trak.header_len..trak.start + trak_header.size,
    )?
    else {
        return Ok(None);
    };
    let Some(moov_header) = peek_box(bytes, moov.start)? else {
        return Ok(None);
    };
    let moov_body = moov.start + moov.header_len..moov.start + moov_header.size;
    let Some((mvex_at, mvex)) = find_child(bytes, moov_body, b"mvex")? else {
        return Ok(None);
    };
    let mvex_body = mvex_at.start + mvex.header_len..mvex_at.start + mvex.size;
    for (trex_at, trex) in find_children(bytes, mvex_body, b"trex")? {
        let payload = trex_at.start + trex.header_len..trex_at.start + trex.size;
        if payload.len() >= 12 && be_u32(bytes, payload.start + 4) == track_id {
            return Ok(Some(be_u32(bytes, payload.start + 8)));
        }
    }
    Ok(None)
}

/// The `hvcC` the init's samples are configured by: the only description, or
/// the first of several decoder-equivalent ones.
fn locate_configuring_hvcc(bytes: &[u8]) -> Result<Option<HvcCLocation>, Fmp4Error> {
    if let Some(mut entries) = equivalent_hevc_sample_entries(bytes)? {
        entries.truncate(1);
        return Ok(entries.pop());
    }
    locate_hvcc(bytes)
}

fn locate_hvcc(bytes: &[u8]) -> Result<Option<HvcCLocation>, Fmp4Error> {
    let mut locations = locate_hevc_sample_entries(bytes)?;
    if locations.len() > 1 {
        return Err(Fmp4Error::MultipleHevcSampleEntries {
            count: locations.len(),
        });
    }
    Ok(locations.pop().flatten())
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) fn duplicate_avc_sample_entry_for_fixture(init: &mut Init) {
    let video_id = init.video().expect("AVC fixture has video").id;
    let (_, locations) =
        locate_avc_sample_entries(&init.bytes, video_id).expect("locating original avcC");
    assert_eq!(locations.len(), 1, "fixture must start with one AVC entry");
    let location = &locations[0];
    let entry_size = peek_box(&init.bytes, location.entry.start)
        .expect("reading AVC sample entry")
        .expect("complete AVC sample entry")
        .size;
    let entry_end = location.entry.start + entry_size;
    let duplicate = init.bytes[location.entry.start..entry_end].to_vec();
    let delta = duplicate.len();
    init.bytes.splice(entry_end..entry_end, duplicate);

    let stsd = location.ancestors[0];
    let entry_count_at = stsd.start + stsd.header_len + 4;
    let entry_count = be_u32(&init.bytes, entry_count_at);
    init.bytes[entry_count_at..entry_count_at + 4]
        .copy_from_slice(&(entry_count + 1).to_be_bytes());
    for &ancestor in &location.ancestors {
        grow_box(&mut init.bytes, ancestor, delta).expect("growing AVC sample-entry ancestors");
    }
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) fn replace_second_avc_triplet_for_fixture(
    init: &mut Init,
    profile: u8,
    compatibility: u8,
    level: u8,
) {
    let video_id = init.video().expect("AVC fixture has video").id;
    let (_, locations) =
        locate_avc_sample_entries(&init.bytes, video_id).expect("locating duplicate avcC");
    assert_eq!(locations.len(), 2, "fixture must have two AVC entries");
    let payload = locations[1].payload.clone();
    init.bytes[payload.start + 1..payload.start + 4].copy_from_slice(&[
        profile,
        compatibility,
        level,
    ]);
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) fn replace_avc_sample_entry_for_fixture(init: &mut Init, sample_entry: [u8; 4]) {
    assert!(
        matches!(&sample_entry, b"avc1" | b"avc3" | b"dva1" | b"dvav"),
        "fixture replacement must remain AVC"
    );
    let video_id = init.video().expect("AVC fixture has video").id;
    let (_, locations) = locate_avc_sample_entries(&init.bytes, video_id).expect("locating avcC");
    assert!(!locations.is_empty(), "fixture has AVC sample entries");
    for location in locations {
        init.bytes[location.entry.start + 4..location.entry.start + 8]
            .copy_from_slice(&sample_entry);
    }
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) fn duplicate_hevc_sample_entry_for_fixture(init: &mut Init) {
    let location = locate_hvcc(&init.bytes)
        .expect("locating the original hvcC")
        .expect("HEVC sample entry");
    let entry = location.ancestors[1];
    let stsd = location.ancestors[2];
    let entry_size = peek_box(&init.bytes, entry.start)
        .expect("reading the HEVC sample entry")
        .expect("complete HEVC sample entry")
        .size;
    let entry_end = entry.start + entry_size;
    let duplicate = init.bytes[entry.start..entry_end].to_vec();
    let delta = duplicate.len();
    init.bytes.splice(entry_end..entry_end, duplicate);

    let entry_count_at = stsd.start + stsd.header_len + 4;
    let entry_count = be_u32(&init.bytes, entry_count_at);
    init.bytes[entry_count_at..entry_count_at + 4]
        .copy_from_slice(&(entry_count + 1).to_be_bytes());
    for &ancestor in &location.ancestors[2..] {
        grow_box(&mut init.bytes, ancestor, delta).expect("growing sample-entry ancestors");
    }
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) fn replace_second_hevc_configuration_for_fixture(init: &mut Init, donor: &Init) {
    let locations = locate_hevc_sample_entries(&init.bytes).expect("locating duplicate entries");
    assert_eq!(locations.len(), 2, "fixture must have two HEVC entries");
    let target = locations[1]
        .as_ref()
        .expect("second HEVC sample entry has hvcC");
    let donor_location = locate_hvcc(&donor.bytes)
        .expect("locating donor hvcC")
        .expect("donor HEVC configuration");
    let replacement = donor.bytes[donor_location.payload].to_vec();
    let old_len = target.payload.len();
    assert_ne!(
        &init.bytes[target.payload.clone()],
        replacement.as_slice(),
        "donor must provide a distinct encoder-produced configuration"
    );
    init.bytes
        .splice(target.payload.clone(), replacement.iter().copied());
    if replacement.len() > old_len {
        let delta = replacement.len() - old_len;
        for &ancestor in &target.ancestors {
            grow_box(&mut init.bytes, ancestor, delta).expect("growing hvcC ancestors");
        }
    } else {
        let delta = old_len - replacement.len();
        for &ancestor in &target.ancestors {
            shrink_box(&mut init.bytes, ancestor, delta).expect("shrinking hvcC ancestors");
        }
    }
}

fn find_child(
    bytes: &[u8],
    range: Range<usize>,
    kind: &[u8; 4],
) -> Result<Option<(BoxAt, BoxHeader)>, Fmp4Error> {
    Ok(find_children(bytes, range, kind)?.into_iter().next())
}

fn find_children(
    bytes: &[u8],
    range: Range<usize>,
    kind: &[u8; 4],
) -> Result<Vec<(BoxAt, BoxHeader)>, Fmp4Error> {
    if range.end > bytes.len() || range.start > range.end {
        return malformed("child-box range runs outside its parent");
    }
    let mut found = Vec::new();
    let mut pos = range.start;
    while pos + 8 <= range.end {
        let Some(header) = peek_box_before(bytes, pos, Some(range.end))? else {
            break;
        };
        let Some(end) = pos.checked_add(header.size) else {
            return malformed("box size overflows while locating hvcC");
        };
        if end > range.end {
            return malformed(format!(
                "box {} runs past its parent while locating hvcC",
                fourcc(header.kind())
            ));
        }
        if header.kind() == kind {
            found.push((
                BoxAt {
                    start: pos,
                    header_len: header.header_len,
                },
                header,
            ));
        }
        pos = end;
    }
    Ok(found)
}

fn hvcc_hdr10_sei_types(record: &[u8]) -> Result<Vec<u16>, Fmp4Error> {
    if record.len() < 23 {
        return malformed("hvcC too short for its NAL arrays");
    }
    let mut found = Vec::new();
    let mut pos = 23usize;
    for _ in 0..record[22] {
        if pos + 3 > record.len() {
            return malformed("hvcC NAL array header runs past the record");
        }
        let array_type = record[pos] & 0x3f;
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]) as usize;
        pos += 3;
        for _ in 0..count {
            if pos + 2 > record.len() {
                return malformed("hvcC NAL length runs past the record");
            }
            let len = u16::from_be_bytes([record[pos], record[pos + 1]]) as usize;
            pos += 2;
            let Some(end) = pos.checked_add(len) else {
                return malformed("hvcC NAL length overflows");
            };
            if end > record.len() {
                return malformed("hvcC NAL runs past the record");
            }
            if array_type == 39 {
                for kind in hdr10_sei_types(&record[pos..end]) {
                    if !found.contains(&kind) {
                        found.push(kind);
                    }
                }
            }
            pos = end;
        }
    }
    if pos != record.len() {
        return malformed("hvcC has trailing bytes after its NAL arrays");
    }
    Ok(found)
}

fn hvcc_nal_array_types(record: &[u8]) -> Result<Vec<u8>, Fmp4Error> {
    hvcc_nal_arrays(record).map(|(present, _)| present)
}

fn hvcc_parameter_set_arrays_complete(record: &[u8]) -> Result<bool, Fmp4Error> {
    let (present, complete) = hvcc_nal_arrays(record)?;
    Ok(complete && (32u8..=34).all(|kind| present.contains(&kind)))
}

/// Parse the same bounded array table for presence and out-of-band completeness.
/// Presence-only promotion must not manufacture a completeness claim for an
/// existing array; hvc1/dvh1 publication requires every parameter-set array's flag.
fn hvcc_nal_arrays(record: &[u8]) -> Result<(Vec<u8>, bool), Fmp4Error> {
    if record.len() < 23 {
        return malformed("hvcC too short for its NAL arrays");
    }
    let mut found = Vec::new();
    let mut complete = true;
    let mut pos = 23usize;
    for _ in 0..record[22] {
        if pos + 3 > record.len() {
            return malformed("hvcC NAL array header runs past the record");
        }
        let array_type = record[pos] & 0x3f;
        if matches!(array_type, 32..=34) && record[pos] & 0x80 == 0 {
            complete = false;
        }
        let count = u16::from_be_bytes([record[pos + 1], record[pos + 2]]) as usize;
        pos += 3;
        for _ in 0..count {
            if pos + 2 > record.len() {
                return malformed("hvcC NAL length runs past the record");
            }
            let len = u16::from_be_bytes([record[pos], record[pos + 1]]) as usize;
            pos += 2;
            pos = pos
                .checked_add(len)
                .filter(|end| *end <= record.len())
                .ok_or_else(|| Fmp4Error::Malformed("hvcC NAL runs past the record".into()))?;
        }
        // A zero-count array names a type but configures nothing. Treating it
        // as present would let a syntactically valid yet undecodable hvcC
        // satisfy the completeness gate and suppress real promotion input.
        if count > 0 && !found.contains(&array_type) {
            found.push(array_type);
        }
    }
    if pos != record.len() {
        return malformed("hvcC has trailing bytes after its NAL arrays");
    }
    Ok((found, complete))
}

fn grow_box(bytes: &mut [u8], at: BoxAt, delta: usize) -> Result<(), Fmp4Error> {
    match at.header_len {
        8 => {
            let old = be_u32(bytes, at.start) as usize;
            let new = old
                .checked_add(delta)
                .and_then(|size| u32::try_from(size).ok())
                .ok_or_else(|| {
                    Fmp4Error::Unsupported("box size exceeds u32 after hvcC enrichment".into())
                })?;
            bytes[at.start..at.start + 4].copy_from_slice(&new.to_be_bytes());
        }
        16 => {
            let old = be_u64(bytes, at.start + 8);
            let new = old.checked_add(delta as u64).ok_or_else(|| {
                Fmp4Error::Unsupported("extended box size overflows after hvcC enrichment".into())
            })?;
            bytes[at.start + 8..at.start + 16].copy_from_slice(&new.to_be_bytes());
        }
        other => {
            return Err(Fmp4Error::Unsupported(format!(
                "cannot grow a box with a {other}-byte header"
            )));
        }
    }
    Ok(())
}

/// The same patch downward, for a box that is losing a child.
///
/// Underflow is an error rather than a saturation: a box that claims to be
/// smaller than the child being removed from it is not a box we understand,
/// and clamping its size to zero would produce an init that parses as a
/// different file.
fn shrink_box(bytes: &mut [u8], at: BoxAt, delta: usize) -> Result<(), Fmp4Error> {
    match at.header_len {
        8 => {
            let old = be_u32(bytes, at.start) as usize;
            let new = old
                .checked_sub(delta)
                .filter(|size| *size >= at.header_len)
                .and_then(|size| u32::try_from(size).ok())
                .ok_or_else(|| {
                    Fmp4Error::Unsupported(
                        "a box is too small to have contained the record being removed".into(),
                    )
                })?;
            bytes[at.start..at.start + 4].copy_from_slice(&new.to_be_bytes());
        }
        16 => {
            let old = be_u64(bytes, at.start + 8);
            let new = old
                .checked_sub(delta as u64)
                .filter(|size| *size >= at.header_len as u64)
                .ok_or_else(|| {
                    Fmp4Error::Unsupported(
                        "an extended box is too small to have contained the record being removed"
                            .into(),
                    )
                })?;
            bytes[at.start + 8..at.start + 16].copy_from_slice(&new.to_be_bytes());
        }
        other => {
            return Err(Fmp4Error::Unsupported(format!(
                "cannot shrink a box with a {other}-byte header"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// moof
// ---------------------------------------------------------------------------

/// Parse a `moof`'s track fragments. Takes the whole box, header included,
/// because every `trun` data offset is relative to its first byte.
fn parse_moof(
    moof: &[u8],
    header_len: usize,
    tracks: &[Track],
) -> Result<Vec<TrackFragment>, Fmp4Error> {
    let mut out = Vec::new();
    let body = &moof[header_len..];
    for (hdr, start, end) in children(body)? {
        if hdr.kind() != b"traf" {
            continue;
        }
        out.push(parse_traf(&body[start..end], header_len + start, tracks)?);
    }
    if out.is_empty() {
        return malformed("moof has no traf");
    }
    Ok(out)
}

fn parse_traf(payload: &[u8], base: usize, tracks: &[Track]) -> Result<TrackFragment, Fmp4Error> {
    let mut track_id = 0u32;
    let mut base_decode_time = 0u64;
    // Seeded from `trex`, overwritten by `tfhd`, overwritten again per sample
    // by `trun` — the ladder ISO 14496-12 §8.8.7 describes, bottom rung first.
    // Filled once the `tfhd` names the track, since which `trex` applies is
    // not known before that.
    let mut default_duration = 0u32;
    let mut default_size = 0u32;
    let mut default_flags = 0u32;
    let mut runs: Vec<Run> = Vec::new();
    // Where a `trun` without an explicit data offset continues from. Relative
    // to the start of the enclosing `moof`, which is exactly what
    // `default-base-is-moof` makes the base.
    let mut running_offset: usize = 0;

    for (hdr, start, end) in children(payload)? {
        let b = &payload[start..end];
        match hdr.kind() {
            b"tfhd" => {
                if b.len() < 8 {
                    return malformed("tfhd too short");
                }
                let flags = be_u32(b, 0) & 0x00ff_ffff;
                track_id = be_u32(b, 4);
                if let Some(t) = tracks.iter().find(|t| t.id == track_id) {
                    default_duration = t.default_sample_duration;
                    default_size = t.default_sample_size;
                    default_flags = t.default_sample_flags;
                }
                if flags & 0x00_0001 != 0 {
                    // An absolute base_data_offset only appears without
                    // `default_base_moof`, which plurx always passes. Refusing
                    // is honest: the value is a file offset a pipe reader
                    // cannot resolve.
                    return Err(Fmp4Error::Unsupported(
                        "tfhd carries an explicit base_data_offset".into(),
                    ));
                }
                let mut p = 8;
                if flags & 0x00_0002 != 0 {
                    p += 4; // sample_description_index
                }
                if flags & 0x00_0008 != 0 {
                    if b.len() < p + 4 {
                        return malformed("tfhd truncated at default_sample_duration");
                    }
                    default_duration = be_u32(b, p);
                    p += 4;
                }
                if flags & 0x00_0010 != 0 {
                    if b.len() < p + 4 {
                        return malformed("tfhd truncated at default_sample_size");
                    }
                    default_size = be_u32(b, p);
                    p += 4;
                }
                if flags & 0x00_0020 != 0 {
                    if b.len() < p + 4 {
                        return malformed("tfhd truncated at default_sample_flags");
                    }
                    default_flags = be_u32(b, p);
                }
            }
            b"tfdt" => {
                if b.is_empty() {
                    return malformed("tfdt too short");
                }
                if b[0] == 1 {
                    if b.len() < 12 {
                        return malformed("tfdt v1 too short");
                    }
                    base_decode_time = be_u64(b, 4);
                } else {
                    if b.len() < 8 {
                        return malformed("tfdt v0 too short");
                    }
                    base_decode_time = be_u32(b, 4) as u64;
                }
            }
            b"trun" => {
                let run = parse_trun(
                    b,
                    base + start,
                    default_duration,
                    default_size,
                    default_flags,
                    running_offset,
                )?;
                running_offset = run.data_offset + run.byte_len();
                runs.push(run);
            }
            _ => {}
        }
    }
    if track_id == 0 {
        return malformed("traf has no tfhd track_id");
    }
    Ok(TrackFragment {
        track_id,
        base_decode_time,
        runs,
    })
}

fn parse_trun(
    b: &[u8],
    base: usize,
    default_duration: u32,
    default_size: u32,
    default_flags: u32,
    continue_from: usize,
) -> Result<Run, Fmp4Error> {
    if b.len() < 8 {
        return malformed("trun too short");
    }
    let version = b[0];
    if version > 1 {
        return malformed("unsupported trun version");
    }
    let flags = be_u32(b, 0) & 0x00ff_ffff;
    let count = be_u32(b, 4) as usize;
    let mut p = 8;
    let mut data_offset_at = None;
    let data_offset = if flags & 0x00_0001 != 0 {
        if b.len() < p + 4 {
            return malformed("trun truncated at data_offset");
        }
        let v = be_u32(b, p) as i32;
        data_offset_at = Some(base + p);
        p += 4;
        if v < 0 {
            return Err(Fmp4Error::Unsupported(
                "trun with a negative data_offset".into(),
            ));
        }
        v as usize
    } else {
        continue_from
    };
    let first_sample_flags = if flags & 0x00_0004 != 0 {
        if b.len() < p + 4 {
            return malformed("trun truncated at first_sample_flags");
        }
        let v = be_u32(b, p);
        p += 4;
        Some(v)
    } else {
        None
    };

    // Both checks are needed. The length check catches a `trun` whose declared
    // samples run past the box; the count cap catches one whose per-sample
    // cost is zero (no per-sample flags set), where any `sample_count`
    // survives the length check and the allocation below is what dies.
    if count > MAX_SAMPLES_PER_RUN {
        return malformed(format!("trun declares {count} samples"));
    }
    let per = 4 * usize::from(flags & 0x00_0100 != 0)
        + 4 * usize::from(flags & 0x00_0200 != 0)
        + 4 * usize::from(flags & 0x00_0400 != 0)
        + 4 * usize::from(flags & 0x00_0800 != 0);
    let need = count.saturating_mul(per);
    if b.len() < p.saturating_add(need) {
        return malformed(format!("trun declares {count} samples it does not carry"));
    }

    let mut samples = Vec::with_capacity(count);
    for i in 0..count {
        let mut duration = default_duration;
        let mut size = default_size;
        let mut sflags = if i == 0 {
            first_sample_flags.unwrap_or(default_flags)
        } else {
            default_flags
        };
        let mut cto = 0i64;
        if flags & 0x00_0100 != 0 {
            duration = be_u32(b, p);
            p += 4;
        }
        let mut size_at = None;
        if flags & 0x00_0200 != 0 {
            size = be_u32(b, p);
            size_at = Some(base + p);
            p += 4;
        }
        if flags & 0x00_0400 != 0 {
            let v = be_u32(b, p);
            p += 4;
            // First-sample-flags win over a per-sample value for sample zero:
            // that is what the flag is for, and ffmpeg only ever sets one.
            if !(i == 0 && first_sample_flags.is_some()) {
                sflags = v;
            }
        }
        if flags & 0x00_0800 != 0 {
            let raw = be_u32(b, p);
            p += 4;
            cto = if version == 0 {
                raw as i64
            } else {
                raw as i32 as i64
            };
        }
        samples.push(Sample {
            duration,
            size,
            size_at,
            flags: sflags,
            cto,
        });
    }
    Ok(Run {
        version,
        composition_offsets_present: flags & 0x00_0800 != 0,
        data_offset,
        data_offset_at,
        samples,
    })
}

/// A refused encoded timeline is never eligible for publication. These fixed
/// reasons distinguish a bad producer clock from a malformed presentation grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncodedGridError {
    #[error("missing selected video track")]
    MissingVideo,
    #[error("video timescale differs from immutable plan")]
    Timescale,
    #[error("selected video track contains edit metadata")]
    EditList,
    #[error("fragment does not begin with clean random access")]
    RandomAccess,
    #[error("decode anchor differs from planned entry")]
    DecodeAnchor,
    #[error("random-access picture does not present at the entry boundary")]
    PresentationAnchor,
    #[error("invalid planned frame duration or entry size")]
    PlanDuration,
    #[error("sample count differs from planned frame count")]
    SampleCount,
    #[error("sample duration differs from declared output cadence")]
    SampleDuration,
    #[error("nonzero composition offset lacks signed version-1 wire shape")]
    CompositionShape,
    #[error("timestamp arithmetic exceeds the representable timeline")]
    Overflow,
    #[error("presentation timestamp is outside the planned interval")]
    PresentationInterval,
    #[error("presentation slots are missing, duplicated or off-grid")]
    PresentationGrid,
}

impl EncodedGridError {
    /// The fixed reason, for refusals that carry only static text.
    pub fn reason(self) -> &'static str {
        match self {
            Self::MissingVideo => "encoded DV window timing: missing selected video track",
            Self::Timescale => "encoded DV window timing: video timescale differs from the plan",
            Self::EditList => "encoded DV window timing: video track contains edit metadata",
            Self::RandomAccess => {
                "encoded DV window timing: fragment does not begin with clean random access"
            }
            Self::DecodeAnchor => {
                "encoded DV window timing: decode anchor differs from the planned entry"
            }
            Self::PresentationAnchor => {
                "encoded DV window timing: first picture does not present at the entry boundary"
            }
            Self::PlanDuration => {
                "encoded DV window timing: invalid planned frame duration or entry size"
            }
            Self::SampleCount => {
                "encoded DV window timing: sample count differs from the planned frame count"
            }
            Self::SampleDuration => {
                "encoded DV window timing: sample duration differs from the output cadence"
            }
            Self::CompositionShape => {
                "encoded DV window timing: composition offset lacks the signed version-1 shape"
            }
            Self::Overflow => "encoded DV window timing: timestamp arithmetic overflow",
            Self::PresentationInterval => {
                "encoded DV window timing: presentation timestamp outside the planned interval"
            }
            Self::PresentationGrid => {
                "encoded DV window timing: presentation slots missing, duplicated or off-grid"
            }
        }
    }
}

/// Prove the exact presentation multiset, independently of decode order. The
/// frame duration comes from the frozen encoder recipe, never the output.
pub fn validate_encoded_grid(
    fragment: &Fragment,
    init: &Init,
    timescale: u32,
    start: u64,
    duration: u64,
    frame_ticks: u32,
) -> Result<(), EncodedGridError> {
    use EncodedGridError as E;
    let track = init.video().ok_or(E::MissingVideo)?;
    let video = fragment.track(track.id).ok_or(E::MissingVideo)?;
    if track.timescale != timescale || timescale == 0 {
        return Err(E::Timescale);
    }
    if track.has_edit_list {
        return Err(E::EditList);
    }
    if !classify(fragment, init).is_clean() {
        return Err(E::RandomAccess);
    }
    if video.base_decode_time != start {
        return Err(E::DecodeAnchor);
    }
    if video.samples().next().is_some_and(|sample| sample.cto != 0) {
        return Err(E::PresentationAnchor);
    }
    let step = u64::from(frame_ticks);
    if step == 0 || duration == 0 || !duration.is_multiple_of(step) {
        return Err(E::PlanDuration);
    }
    if u64::try_from(video.sample_count()).map_err(|_| E::Overflow)? != duration / step {
        return Err(E::SampleCount);
    }
    let end = start.checked_add(duration).ok_or(E::Overflow)?;
    let mut dts = i128::from(start);
    let mut presentation = Vec::with_capacity(video.sample_count());
    for run in &video.runs {
        for sample in &run.samples {
            if sample.duration != frame_ticks {
                return Err(E::SampleDuration);
            }
            if sample.cto != 0
                && (run.version != 1
                    || !run.composition_offsets_present
                    || i32::try_from(sample.cto).is_err())
            {
                return Err(E::CompositionShape);
            }
            let pts = dts.checked_add(i128::from(sample.cto)).ok_or(E::Overflow)?;
            if pts < i128::from(start) || pts >= i128::from(end) {
                return Err(E::PresentationInterval);
            }
            presentation.push(pts);
            dts = dts
                .checked_add(i128::from(sample.duration))
                .ok_or(E::Overflow)?;
        }
    }
    if dts != i128::from(end) {
        return Err(E::SampleDuration);
    }
    presentation.sort_unstable();
    let mut expected = i128::from(start);
    for pts in presentation {
        if pts != expected {
            return Err(E::PresentationGrid);
        }
        expected = expected
            .checked_add(i128::from(frame_ticks))
            .ok_or(E::Overflow)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// classification
// ---------------------------------------------------------------------------

/// Whether a fragment's first frame is safe to start a segment with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutClass {
    /// An IDR. Nothing before it in decode order presents after it; nothing
    /// to discard.
    CleanIdr,
    /// A CRA with no leading pictures. A random-access decoder finds nothing
    /// to throw away here either.
    CleanCra,
    /// A CRA (or BLA) carrying leading pictures — a segment starting here
    /// loses exactly one frame per boundary, which is the whole bug.
    Dirty,
    /// The fragment could not be read. Treated as [`CutClass::Dirty`]
    /// everywhere a cut is considered, and counted separately so a stream this
    /// reader does not understand shows up as a number rather than as silence.
    Unparseable,
}

impl CutClass {
    /// True when a segment may begin in front of this fragment.
    pub fn is_clean(self) -> bool {
        matches!(self, CutClass::CleanIdr | CutClass::CleanCra)
    }

    pub fn label(self) -> &'static str {
        match self {
            CutClass::CleanIdr => "idr",
            CutClass::CleanCra => "cra",
            CutClass::Dirty => "dirty",
            CutClass::Unparseable => "unparseable",
        }
    }
}

/// Classify a fragment's opening keyframe.
///
/// The rule, from `docs/streaming/SEGMENTER-PLAN.md` §4.2, is conservative in one
/// direction only: anything not positively identified as clean is dirty, so a
/// misread costs a boundary that could have been cleaner and never costs a
/// frame that should have survived.
pub fn classify(frag: &Fragment, init: &Init) -> CutClass {
    let Some(video) = init.video() else {
        return CutClass::Unparseable;
    };
    let Some(tf) = frag.track(video.id) else {
        return CutClass::Unparseable;
    };
    let Some(run) = tf.runs.first() else {
        return CutClass::Unparseable;
    };
    let Some(first) = run.samples.first() else {
        return CutClass::Unparseable;
    };
    let Some(codec) = video.codec else {
        return CutClass::Unparseable;
    };
    if video.nal_length_size == 0 {
        return CutClass::Unparseable;
    }
    let start = run.data_offset;
    let end = start.saturating_add(first.size as usize);
    if end > frag.bytes.len() {
        return CutClass::Unparseable;
    }
    let Some(nal) = first_vcl_nal(&frag.bytes[start..end], video.nal_length_size, codec) else {
        return CutClass::Unparseable;
    };
    match codec {
        VideoCodec::Hevc => match nal {
            // IDR_W_RADL / IDR_N_LP — closed, by definition.
            19 | 20 => CutClass::CleanIdr,
            // CRA_NUT and the three BLA types: IRAPs that MAY carry leading
            // pictures, so the bitstream has to be asked rather than assumed.
            16..=18 | 21 => {
                if has_leading_picture(tf) {
                    CutClass::Dirty
                } else {
                    CutClass::CleanCra
                }
            }
            // A fragment opening on a non-IRAP VCL NAL should not happen under
            // `frag_keyframe`; if it does, it is emphatically not a cut point.
            _ => CutClass::Dirty,
        },
        VideoCodec::H264 => {
            if nal == 5 {
                CutClass::CleanIdr
            } else {
                // x264's open GOP signals recovery points rather than a
                // distinct NAL type, so only an IDR is provably clean here.
                CutClass::Dirty
            }
        }
    }
}

/// The first VCL NAL in a length-prefixed sample — SEI, AUD and any parameter
/// sets that survived the strip are stepped over, and the first slice decides.
fn first_vcl_nal(sample: &[u8], length_size: u8, codec: VideoCodec) -> Option<u8> {
    let lsz = length_size as usize;
    let mut q = 0usize;
    while q + lsz < sample.len() {
        let mut nlen = 0usize;
        for i in 0..lsz {
            nlen = (nlen << 8) | sample[q + i] as usize;
        }
        let head = *sample.get(q + lsz)?;
        let ty = match codec {
            VideoCodec::Hevc => (head >> 1) & 0x3f,
            VideoCodec::H264 => head & 0x1f,
        };
        let is_vcl = match codec {
            VideoCodec::Hevc => ty <= 31,
            VideoCodec::H264 => (1..=5).contains(&ty),
        };
        if is_vcl {
            return Some(ty);
        }
        if nlen == 0 {
            return None;
        }
        q = q.checked_add(lsz)?.checked_add(nlen)?;
    }
    None
}

/// Does any sample in this fragment present before its first one?
///
/// That is the whole definition of a leading picture, and it is measured in
/// presentation time rather than in composition offsets: ffmpeg shifts dts so
/// every offset is non-negative, so `pts(0)` is emphatically not the minimum
/// on a dirty fragment — which is exactly the point.
fn has_leading_picture(tf: &TrackFragment) -> bool {
    let mut dts = 0i64;
    let mut first_pts: Option<i64> = None;
    for s in tf.samples() {
        let pts = dts + s.cto;
        match first_pts {
            None => first_pts = Some(pts),
            Some(p0) if pts < p0 => return true,
            _ => {}
        }
        dts += s.duration as i64;
    }
    false
}

// ---------------------------------------------------------------------------
// cut policy
// ---------------------------------------------------------------------------

/// Why a segment ended, so the session can report honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutReason {
    /// Past the duration floor and the next fragment opens on a true
    /// random-access point. The good case, and the reason this exists.
    Clean,
    /// The accumulated bytes reached the ceiling before any clean point
    /// appeared. The cut is taken anyway — a segment that never ends is worse
    /// than a dropped frame — and counted.
    ByteCeiling,
    /// Same, for the duration ceiling that keeps `TARGETDURATION` sane on
    /// low-bitrate sources.
    TimeCeiling,
    /// End of stream: whatever is left becomes the last segment.
    EndOfStream,
}

impl CutReason {
    pub fn is_clean(self) -> bool {
        self == CutReason::Clean
    }

    pub fn label(self) -> &'static str {
        match self {
            CutReason::Clean => "clean",
            CutReason::ByteCeiling => "byte-ceiling",
            CutReason::TimeCeiling => "time-ceiling",
            CutReason::EndOfStream => "eof",
        }
    }
}

/// When to end a segment. Pure, so the whole policy is one unit test away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPolicy {
    /// Floor, in video timescale ticks, before a clean cut may be taken.
    pub floor_ticks: u64,
    /// The same, for the FIRST segment only, and much lower.
    ///
    /// A player's runway at startup is exactly the first segment, because that
    /// is all the playlist holds; its next chance to learn any other segment
    /// exists is one `TARGETDURATION` away. So the first segment has to be
    /// short enough that the first reload lands with time to spare — and by
    /// then the burst has filled the playlist and the question never arises
    /// again. It also puts the first frame on screen sooner, since nothing
    /// plays until one whole segment exists.
    ///
    /// The cost is one extra boundary per session, and usually not even that:
    /// the cut still has to land on a clean keyframe, and on a source with
    /// clean points every couple of seconds it will.
    pub first_floor_ticks: u64,
    /// Hard ceiling in bytes. Binding at 69 Mb/s long before any duration is.
    pub max_bytes: usize,
    /// Secondary ceiling in ticks, for sources whose bytes never pile up.
    pub max_ticks: u64,
    /// Optional strict bound for an unplanned rolling presentation.
    ///
    /// This is deliberately absent from persisted immutable plans: their
    /// boundaries and covering target were already decided together. A live
    /// segmenter uses it to prove a fixed advertised target before any media
    /// can be exposed.
    pub strict_max_ticks: Option<u64>,
}

impl CutPolicy {
    /// Build from the shipped constants and the stream's own video timescale.
    pub fn new(
        floor_seconds: u32,
        first_floor_seconds: u32,
        max_bytes: usize,
        max_seconds: u32,
        timescale: u32,
    ) -> CutPolicy {
        let ts = timescale.max(1) as u64;
        CutPolicy {
            floor_ticks: floor_seconds as u64 * ts,
            first_floor_ticks: first_floor_seconds.min(floor_seconds) as u64 * ts,
            max_bytes,
            max_ticks: max_seconds as u64 * ts,
            strict_max_ticks: None,
        }
    }

    /// Add the strict bound used by an unplanned rolling presentation.
    pub fn with_strict_max_seconds(mut self, strict_max_seconds: u32, timescale: u32) -> Self {
        self.strict_max_ticks = Some(strict_max_seconds as u64 * timescale.max(1) as u64);
        self
    }

    /// Should the fragments accumulated so far be published *before* the
    /// fragment that just arrived? `None` means keep accumulating.
    ///
    /// **The floor gates everything, including the ceilings**, and that order
    /// is load-bearing rather than tidy. `COPY_SEGMENT_MAX_BYTES` is 48 MB and
    /// the reference 4K remux runs at 69 Mb/s, which reaches 48 MB in 5.6
    /// seconds — so a ceiling allowed to fire on its own would cut *below* the
    /// six-second floor and produce MORE boundaries than the muxer this
    /// replaces, on exactly the file the whole investigation is about. Every
    /// boundary costs a frame; a segmenter that adds boundaries to a source
    /// with no clean point would be worse than doing nothing, which is the one
    /// thing this is not allowed to be. The floor is the measured decision
    /// (`e212c55`, which weighed 6 s ≈ 52 MB at 69 Mb/s and took it); the
    /// ceilings bound how long we are willing to wait *past* it for a clean
    /// point, and nothing else.
    ///
    /// After the floor, a clean cut wins over both ceilings — so a boundary
    /// that could be clean is never spent as a ceiling cut, and a ceiling that
    /// happens to land in front of a clean fragment is counted clean, because
    /// the ceiling decided only *when*, not *where*.
    pub fn cut_before(
        &self,
        pending_ticks: u64,
        pending_bytes: usize,
        next: CutClass,
        first_segment: bool,
    ) -> Option<CutReason> {
        let floor = if first_segment {
            self.first_floor_ticks
        } else {
            self.floor_ticks
        };
        if pending_bytes == 0 || pending_ticks < floor {
            return None;
        }
        if next.is_clean() {
            return Some(CutReason::Clean);
        }
        if pending_bytes >= self.max_bytes {
            return Some(CutReason::ByteCeiling);
        }
        if pending_ticks >= self.max_ticks {
            return Some(CutReason::TimeCeiling);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

/// What merging a run of fragments cost in fidelity. Anything non-zero here is
/// worth a log line; on a stream ffmpeg produced in one pass it is all zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeStats {
    /// Times a source `tfdt` did not equal the previous fragment's end and the
    /// join had to be absorbed into a sample duration.
    pub tfdt_adjustments: u32,
}

/// The merged segment, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub bytes: Vec<u8>,
    pub stats: MergeStats,
    /// Video duration in the video track's timescale — the number `EXTINF` is
    /// written from.
    pub video_ticks: u64,
}

/// One sample's source bytes and the transformation the merged `mdat` needs.
struct Slice {
    /// Index into the merged fragment list.
    fragment: usize,
    /// Byte range within that fragment's own bytes.
    start: usize,
    source_len: usize,
    output_len: usize,
    /// Length-prefix width when an out-of-band HEVC sample entry requires
    /// VPS/SPS/PPS removal; `None` means copy the sample verbatim.
    strip_hevc_parameter_sets: Option<u8>,
}

/// One track's whole contribution to the merged segment: every sample in
/// order, and the byte slices they live in.
struct Plan {
    track_id: u32,
    base_decode_time: u64,
    samples: Vec<Sample>,
    slices: Vec<Slice>,
}

impl Plan {
    fn byte_len(&self) -> usize {
        self.slices.iter().map(|s| s.output_len).sum()
    }
}

fn visit_length_prefixed_hevc_nals(
    sample: &[u8],
    length_size: u8,
    mut visit: impl FnMut(&[u8], Option<u8>),
) -> Result<(), Fmp4Error> {
    let length_size = usize::from(length_size);
    if !(1..=4).contains(&length_size) {
        return malformed("HEVC sample has an invalid NAL length size");
    }
    let mut pos = 0usize;
    while pos < sample.len() {
        let prefix_end = pos
            .checked_add(length_size)
            .filter(|end| *end <= sample.len())
            .ok_or_else(|| Fmp4Error::Malformed("HEVC NAL length is truncated".into()))?;
        let mut nal_len = 0usize;
        for byte in &sample[pos..prefix_end] {
            nal_len = nal_len
                .checked_mul(256)
                .and_then(|value| value.checked_add(usize::from(*byte)))
                .ok_or_else(|| Fmp4Error::Malformed("HEVC NAL length overflows".into()))?;
        }
        if nal_len == 0 {
            return malformed("HEVC sample carries a zero-length NAL unit");
        }
        let end = prefix_end
            .checked_add(nal_len)
            .filter(|end| *end <= sample.len())
            .ok_or_else(|| Fmp4Error::Malformed("HEVC NAL runs past its sample".into()))?;
        let nal = &sample[prefix_end..end];
        visit(&sample[pos..end], hevc_nal_type(nal));
        pos = end;
    }
    Ok(())
}

fn hevc_sample_without_parameter_sets_len(
    sample: &[u8],
    length_size: u8,
) -> Result<usize, Fmp4Error> {
    let mut kept = 0usize;
    visit_length_prefixed_hevc_nals(sample, length_size, |framed, kind| {
        if !matches!(kind, Some(32..=34)) {
            kept = kept.saturating_add(framed.len());
        }
    })?;
    if kept == 0 {
        return Err(Fmp4Error::Unsupported(
            "an HEVC video sample contains parameter sets but no picture data".into(),
        ));
    }
    Ok(kept)
}

fn write_hevc_sample_without_parameter_sets(
    out: &mut Vec<u8>,
    sample: &[u8],
    length_size: u8,
) -> Result<(), Fmp4Error> {
    visit_length_prefixed_hevc_nals(sample, length_size, |framed, kind| {
        if !matches!(kind, Some(32..=34)) {
            out.extend_from_slice(framed);
        }
    })
}

/// Merge consecutive fragments into one `styp moof mdat` HLS segment.
///
/// **The shape is ffmpeg's HLS muxer's shape, deliberately** — `styp`, one
/// `sidx` per track, `moof` with one `traf` and one `trun` per track, `mdat`.
///
/// The first cut of this diverged twice: it omitted the `sidx` boxes (optional
/// in HLS fMP4, and hls.js's passthrough ignores them) and it wrote one `trun`
/// per source fragment rather than one per track, because that let each source
/// `mdat` payload be copied whole. Chrome did not care about either. Safari
/// refused the stream outright and the player's error fallback re-encoded a 4K
/// remux down to 1080p — the exact failure this path exists to prevent, on the
/// exact browser it exists for.
///
/// So the structural divergence is gone. Each track's samples are written into
/// the merged `mdat` consecutively, which makes its whole contribution
/// contiguous and one data offset enough for it; the `sidx` boxes are
/// byte-for-byte the shape ffmpeg writes, `earliest_presentation_time`
/// included. One sample-level normalization is deliberate: after promotion
/// puts VPS/SPS/PPS in hvcC, `hvc1`/`dvh1` samples shed those NALs so the
/// stream does not violate the out-of-band-only promise that fixed the
/// documented 4K boundary-stutter condition. `hev1`/`dvhe` samples remain
/// byte-for-byte unchanged.
///
/// The one thing still normalized rather than copied: `tfhd` carries only
/// `default-base-is-moof` and every sample writes its own duration, size,
/// flags and composition offset, where ffmpeg leans on `tfhd` defaults. That
/// is a density choice, not a structural one — the fully-explicit form is the
/// most-exercised path in every MP4 parser there is — and it keeps the
/// default-value and first-sample-flags ladders confined to the reader.
pub fn merge(fragments: &[Fragment], init: &Init, sequence: u32) -> Result<Segment, Fmp4Error> {
    merge_retaining(fragments, init, sequence, false)
}

/// [`merge`], told whether the video samples' in-band HEVC parameter sets must
/// survive it.
///
/// `true` for a source whose in-band sets redefine the sample entry's record
/// (`CopyVideoOptions::retains_hevc_parameter_sets`): those definitions are
/// the only correct ones for the pictures that follow them, so the
/// out-of-band-only normalization above would reintroduce exactly the
/// corruption the copy's bitstream filter was told to avoid. Every sample is
/// then copied byte for byte, as it already is for `hev1`/`dvhe`.
pub fn merge_retaining(
    fragments: &[Fragment],
    init: &Init,
    sequence: u32,
    retain_parameter_sets: bool,
) -> Result<Segment, Fmp4Error> {
    if fragments.is_empty() {
        return malformed("nothing to merge");
    }
    let mut stats = MergeStats::default();
    let strip_hevc_track = match init.video() {
        Some(video)
            if !retain_parameter_sets
                && video.codec == Some(VideoCodec::Hevc)
                && video.nal_length_size != 0 =>
        {
            locate_hvcc(&init.bytes)?
                .filter(|location| !location.parameter_sets_in_band)
                .map(|_| (video.id, video.nal_length_size))
        }
        _ => None,
    };

    // The UNION of the tracks in the run, in first-seen order — which is
    // ffmpeg's own interleave order, video first, so a diff against hlsenc's
    // output stays readable.
    //
    // The union, and not `fragments[0]`'s tracks, because ffmpeg does not
    // guarantee every fragment carries every track: `mov_flush_fragment`
    // writes a `traf` only for tracks with buffered samples, and once a track
    // goes `max_interleave_delta` (10 s) without a packet the muxer stops
    // waiting for it. A source with a late-starting or gapped audio track
    // therefore produces video-only fragments followed by fragments that
    // carry audio again — and taking the track list from the first of those
    // published a SILENT segment whose audio bytes were still copied into the
    // mdat, with no error and no counter anywhere. Found by review.
    let mut track_ids: Vec<u32> = Vec::new();
    for frag in fragments {
        for t in &frag.tracks {
            if !track_ids.contains(&t.track_id) {
                track_ids.push(t.track_id);
            }
        }
    }
    let mut plans: Vec<Plan> = Vec::with_capacity(track_ids.len());

    for &id in &track_ids {
        let mut samples: Vec<Sample> = Vec::new();
        let mut slices: Vec<Slice> = Vec::new();
        let mut base: Option<u64> = None;
        let mut expected_next: Option<u64> = None;
        for (fi, frag) in fragments.iter().enumerate() {
            let Some(tf) = frag.track(id) else { continue };
            match base {
                None => base = Some(tf.base_decode_time),
                Some(_) => {
                    // One `traf` carries one `tfdt`, so every later fragment's
                    // decode time has to be reachable by summing durations. It
                    // always is on a stream ffmpeg produced in one pass; when
                    // it is not, the join goes into the previous fragment's
                    // last sample rather than silently shifting everything
                    // after it.
                    if let Some(exp) = expected_next {
                        if tf.base_decode_time != exp {
                            let delta = tf.base_decode_time as i64 - exp as i64;
                            if let Some(last) = samples.last_mut() {
                                let adjusted = last.duration as i64 + delta;
                                if (0..=u32::MAX as i64).contains(&adjusted) {
                                    last.duration = adjusted as u32;
                                    stats.tfdt_adjustments += 1;
                                }
                            }
                        }
                    }
                }
            }
            let mut dur = 0u64;
            for run in &tf.runs {
                if run.samples.is_empty() {
                    continue;
                }
                dur += run.samples.iter().map(|s| s.duration as u64).sum::<u64>();
                let strip_length_size = strip_hevc_track
                    .filter(|(track_id, _)| *track_id == id)
                    .map(|(_, length_size)| length_size);
                let mut start = run.data_offset;
                for source_sample in &run.samples {
                    let source_len = source_sample.size as usize;
                    let end = start
                        .checked_add(source_len)
                        .filter(|end| *end <= frag.bytes.len())
                        .ok_or_else(|| {
                            Fmp4Error::Malformed(
                                "a sample points past the end of its fragment".into(),
                            )
                        })?;
                    let output_len = match strip_length_size {
                        Some(length_size) => hevc_sample_without_parameter_sets_len(
                            &frag.bytes[start..end],
                            length_size,
                        )?,
                        None => source_len,
                    };
                    let mut sample = *source_sample;
                    sample.size = u32::try_from(output_len).map_err(|_| {
                        Fmp4Error::Unsupported(
                            "a normalized HEVC sample exceeds the 32-bit MP4 size".into(),
                        )
                    })?;
                    samples.push(sample);
                    slices.push(Slice {
                        fragment: fi,
                        start,
                        source_len,
                        output_len,
                        strip_hevc_parameter_sets: strip_length_size,
                    });
                    start = end;
                }
            }
            expected_next = Some(tf.base_decode_time + dur);
        }
        if let (Some(base), false) = (base, samples.is_empty()) {
            plans.push(Plan {
                track_id: id,
                base_decode_time: base,
                samples,
                slices,
            });
        }
    }
    if plans.is_empty() {
        return malformed("no track survived the merge");
    }

    // Build the moof with zeroed data offsets to learn its size. Every field
    // that could change the size is fixed-width, so the patch below only
    // changes values — the layout is already final.
    let mut offset_slots: Vec<usize> = Vec::new();
    let mut moof = build_moof(&plans, sequence, &mut offset_slots);
    let moof_size = moof.len();

    let payload_total: usize = plans.iter().map(|p| p.byte_len()).sum();
    let Some(mdat_size) = payload_total.checked_add(8) else {
        return malformed("merged mdat size overflows");
    };
    if mdat_size > u32::MAX as usize {
        return Err(Fmp4Error::Unsupported(format!(
            "merged mdat would be {mdat_size} bytes"
        )));
    }

    // Each track's data is contiguous in the merged mdat, which is what lets
    // its whole contribution be one `trun` with one data offset.
    let mut base = 0usize;
    for (i, plan) in plans.iter().enumerate() {
        let offset = moof_size + 8 + base;
        if offset > i32::MAX as usize {
            return Err(Fmp4Error::Unsupported(
                "merged segment exceeds a 32-bit trun data offset".into(),
            ));
        }
        let pos = offset_slots[i];
        moof[pos..pos + 4].copy_from_slice(&(offset as u32).to_be_bytes());
        base += plan.byte_len();
    }

    let sidx_total = plans.len() * SIDX_BYTES;
    let referenced_size = moof_size + mdat_size;
    let mut out = Vec::with_capacity(STYP.len() + sidx_total + referenced_size);
    out.extend_from_slice(&STYP);
    for (i, plan) in plans.iter().enumerate() {
        let timescale = init
            .track(plan.track_id)
            .map(|t| t.timescale)
            .unwrap_or(1)
            .max(1);
        let duration: u64 = plan.samples.iter().map(|s| s.duration as u64).sum();
        // Every sidx after this one sits between it and the material it
        // describes, which is what `first_offset` measures.
        let first_offset = ((plans.len() - 1 - i) * SIDX_BYTES) as u64;
        write_sidx(
            &mut out,
            plan.track_id,
            timescale,
            plan.base_decode_time,
            first_offset,
            referenced_size as u32,
            duration.min(u32::MAX as u64) as u32,
        );
    }
    out.extend_from_slice(&moof);
    out.extend_from_slice(&(mdat_size as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for plan in &plans {
        for slice in &plan.slices {
            let frag = &fragments[slice.fragment];
            let end = slice.start.saturating_add(slice.source_len);
            if end > frag.bytes.len() {
                return malformed("a sample points past the end of its fragment");
            }
            let source = &frag.bytes[slice.start..end];
            if let Some(length_size) = slice.strip_hevc_parameter_sets {
                let before = out.len();
                write_hevc_sample_without_parameter_sets(&mut out, source, length_size)?;
                if out.len() - before != slice.output_len {
                    return malformed("HEVC sample normalization changed size between passes");
                }
            } else {
                out.extend_from_slice(source);
            }
        }
    }

    let video_ticks = init
        .video()
        .and_then(|v| plans.iter().find(|p| p.track_id == v.id))
        .map(|p| p.samples.iter().map(|s| s.duration as u64).sum())
        .unwrap_or(0);

    Ok(Segment {
        bytes: out,
        stats,
        video_ticks,
    })
}

/// A version-1 `sidx` is 52 bytes: the box header, the full-box header, two
/// 32-bit ids, two 64-bit times, the reserved/count pair, and one 12-byte
/// reference.
const SIDX_BYTES: usize = 52;

/// One `sidx`, in exactly the shape ffmpeg's HLS muxer writes — version 1,
/// one reference covering the whole `moof`+`mdat`, `earliest_presentation_time`
/// set to the track's `tfdt` (which is what ffmpeg puts there; it is a decode
/// time, not the true earliest presentation time, and every player that has
/// ever consumed this path has consumed that).
///
/// `starts_with_SAP` is 1 with `SAP_type` 0, again copying ffmpeg rather than
/// getting clever: a segment this module publishes starts on a random-access
/// point by construction on the clean path, and on a ceiling cut it starts on
/// an IRAP whose leading pictures a player discards — the same thing ffmpeg's
/// own segments have always declared here.
fn write_sidx(
    out: &mut Vec<u8>,
    track_id: u32,
    timescale: u32,
    earliest_presentation_time: u64,
    first_offset: u64,
    referenced_size: u32,
    subsegment_duration: u32,
) {
    out.extend_from_slice(&(SIDX_BYTES as u32).to_be_bytes());
    out.extend_from_slice(b"sidx");
    out.extend_from_slice(&0x0100_0000u32.to_be_bytes()); // version 1, flags 0
    out.extend_from_slice(&track_id.to_be_bytes());
    out.extend_from_slice(&timescale.to_be_bytes());
    out.extend_from_slice(&earliest_presentation_time.to_be_bytes());
    out.extend_from_slice(&first_offset.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&1u16.to_be_bytes()); // reference_count
                                                // reference_type 0 (media) | referenced_size
    out.extend_from_slice(&(referenced_size & 0x7fff_ffff).to_be_bytes());
    out.extend_from_slice(&subsegment_duration.to_be_bytes());
    // starts_with_SAP 1 | SAP_type 0 | SAP_delta_time 0
    out.extend_from_slice(&0x8000_0000u32.to_be_bytes());
}

/// The 24-byte `styp` hlsenc writes, copied exactly: major brand `msdh`,
/// compatible with `msdh` and `msix`.
const STYP: [u8; 24] = [
    0, 0, 0, 24, b's', b't', b'y', b'p', b'm', b's', b'd', b'h', 0, 0, 0, 0, b'm', b's', b'd',
    b'h', b'm', b's', b'i', b'x',
];

/// Video runs get composition offsets; audio does not. Decided per run by
/// whether any sample actually carries one, so an audio track never pays four
/// bytes a sample for a column of zeros.
fn trun_flags(samples: &[Sample]) -> u32 {
    // data-offset + duration + size + flags, all present, for every sample.
    let base = 0x0000_0701;
    if samples.iter().any(|s| s.cto != 0) {
        base | 0x0000_0800
    } else {
        base
    }
}

/// A `trun` is version 1 exactly when it has to be: version 0 composition
/// offsets are UNSIGNED.
///
/// ffmpeg shifts dts to keep every offset non-negative unless it is asked for
/// `+negative_cts_offsets`, used by the software-H.264 reordered VOD recipe.
/// Preserve its signed offsets, because the alternative was writing
/// `cto.max(0)` into a version-0 field: a negative offset would then be
/// silently clamped, shifting that frame's presentation time by the offset it
/// lost, with no error — which is exactly the class of change the framemd5
/// equality exists to forbid, arriving without tripping it. Found by review.
fn trun_version(samples: &[Sample]) -> u8 {
    u8::from(samples.iter().any(|s| s.cto < 0))
}

/// Serialize the `moof`, recording where each `trun`'s data offset field
/// landed so the caller can patch it once the total size is known.
fn build_moof(plans: &[Plan], sequence: u32, offset_slots: &mut Vec<usize>) -> Vec<u8> {
    let mut moof: Vec<u8> = Vec::new();
    moof.extend_from_slice(&[0, 0, 0, 0]); // size, patched at the end
    moof.extend_from_slice(b"moof");
    moof.extend_from_slice(&16u32.to_be_bytes());
    moof.extend_from_slice(b"mfhd");
    moof.extend_from_slice(&0u32.to_be_bytes());
    moof.extend_from_slice(&sequence.to_be_bytes());

    for plan in plans {
        let traf_start = moof.len();
        moof.extend_from_slice(&[0, 0, 0, 0]);
        moof.extend_from_slice(b"traf");
        // tfhd: default-base-is-moof and nothing else. Every default it could
        // carry is written per sample instead.
        moof.extend_from_slice(&16u32.to_be_bytes());
        moof.extend_from_slice(b"tfhd");
        moof.extend_from_slice(&0x0002_0000u32.to_be_bytes());
        moof.extend_from_slice(&plan.track_id.to_be_bytes());
        // tfdt v1 — 64-bit, because a long film in a 90 kHz timescale outgrows
        // 32 bits, and because it is what ffmpeg writes.
        moof.extend_from_slice(&20u32.to_be_bytes());
        moof.extend_from_slice(b"tfdt");
        moof.extend_from_slice(&0x0100_0000u32.to_be_bytes());
        moof.extend_from_slice(&plan.base_decode_time.to_be_bytes());

        // ONE trun per traf. Not a style choice: ffmpeg's HLS muxer writes
        // exactly one, and a segment carrying seven of them is a shape Safari
        // has never been asked to read on this path. Getting there costs
        // nothing — each track's slices are copied into the merged mdat
        // consecutively, so its whole contribution is contiguous and one data
        // offset addresses all of it.
        let flags = trun_flags(&plan.samples);
        let version = trun_version(&plan.samples);
        let has_cto = flags & 0x0000_0800 != 0;
        let per = if has_cto { 16 } else { 12 };
        let size = 8 + 4 + 4 + 4 + plan.samples.len() * per;
        moof.extend_from_slice(&(size as u32).to_be_bytes());
        moof.extend_from_slice(b"trun");
        moof.extend_from_slice(&(((version as u32) << 24) | flags).to_be_bytes());
        moof.extend_from_slice(&(plan.samples.len() as u32).to_be_bytes());
        offset_slots.push(moof.len());
        moof.extend_from_slice(&0u32.to_be_bytes()); // data_offset, patched
        for s in &plan.samples {
            moof.extend_from_slice(&s.duration.to_be_bytes());
            moof.extend_from_slice(&s.size.to_be_bytes());
            moof.extend_from_slice(&s.flags.to_be_bytes());
            if has_cto {
                // Written in whichever width the values need, and clamped in
                // neither: version 0 is unsigned, version 1 signed.
                let clamped = s.cto.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                moof.extend_from_slice(&clamped.to_be_bytes());
            }
        }
        let traf_len = (moof.len() - traf_start) as u32;
        moof[traf_start..traf_start + 4].copy_from_slice(&traf_len.to_be_bytes());
    }
    let total = moof.len() as u32;
    moof[0..4].copy_from_slice(&total.to_be_bytes());
    moof
}

// ---------------------------------------------------------------------------
// the segmenter
// ---------------------------------------------------------------------------

/// What a session's segmenter has done so far. This is the end-of-session log
/// line, and `scripts/perf-report` greps it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentCounts {
    pub fragments: u64,
    pub segments: u64,
    /// Cuts taken in front of a true random-access point — the ones that cost
    /// no frame.
    pub clean_cuts: u64,
    /// Cuts forced by the byte or duration ceiling with no clean point in
    /// reach. Each one still costs the leading picture, and saying so is the
    /// difference between a measured residual and a silent one.
    pub ceiling_cuts: u64,
    /// Fragments this reader could not classify. Never cut in front of;
    /// counted because a stream shape nobody anticipated should show up as a
    /// number rather than as silence.
    pub unparseable: u64,
    pub tfdt_adjustments: u32,
}

/// One segment, ready to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Published {
    pub index: u64,
    pub segment: Segment,
    pub reason: CutReason,
    /// `EXTINF`, normally from summed video sample durations in the video
    /// timescale. The final segment uses its longest track when audio outlasts
    /// the last video frame.
    pub seconds: f64,
}

impl Published {
    pub fn name(&self) -> String {
        segment_name(self.index)
    }
}

/// Convert an interval between track clocks without rounding it shorter. The
/// ceiling is a safety boundary, so losing a fractional destination tick here
/// could move a sample that starts exactly on the boundary into the preceding
/// segment.
fn scale_ticks(ticks: u64, from_timescale: u32, to_timescale: u32) -> u64 {
    let from = from_timescale.max(1) as u128;
    let scaled = (ticks as u128)
        .saturating_mul(to_timescale.max(1) as u128)
        .saturating_add(from - 1)
        / from;
    scaled.min(u64::MAX as u128) as u64
}

/// Scale a signed interval between track clocks.
///
/// The magnitude goes through [`scale_ticks`], which rounds up, so the result
/// rounds *away from zero in both directions* — not towards zero on the
/// negative side. That asymmetry is deliberate and it is why the video track
/// is anchored exactly rather than scaled: video's source and destination
/// timescales are the same, so its shift is exact, and every other track's is
/// off by under one of its own ticks — about 20 µs at 48 kHz. The shift is
/// applied once per generation rather than per fragment, so it cannot
/// accumulate, but it is a real constant offset and calling it zero would be
/// wrong.
fn scale_signed(ticks: i64, from_timescale: u32, to_timescale: u32) -> i64 {
    let magnitude = scale_ticks(ticks.unsigned_abs(), from_timescale, to_timescale);
    let magnitude = magnitude.min(i64::MAX as u64) as i64;
    if ticks < 0 {
        -magnitude
    } else {
        magnitude
    }
}

/// Where a generation's media belongs on the film's own timeline.
///
/// A producer that repositioned opens ffmpeg with `-noaccurate_seek -ss` and
/// gets decode times that are not film time in any configuration M0 tested
/// (plan §12.4) — they are whatever the source's timestamps imply, which is
/// why the landing matcher exists at all. But the producer *knows* the film
/// time of what it is about to receive: it is the plan entry it repositioned
/// to, discarded forward to. So the shift is learned once, from the first
/// fragment that carries video, and applied to every fragment for the life of
/// the generation — including the ones that arrived before the anchor did.
///
/// Those leading fragments are the case worth spelling out. ffmpeg writes a
/// `traf` only for tracks with buffered samples, so a generation can open with
/// audio-only fragments. Letting those through unshifted would publish a
/// segment whose audio sits on ffmpeg's clock and whose video sits on the
/// film's — five seconds apart in the fixture this was measured on — and
/// [`merge`]'s discontinuity repair would absorb the difference into a sample
/// duration without always counting it. So they wait in `pending` and are
/// rebased with everything else. Nothing can be published before the anchor
/// arrives, because a cut requires `pending_ticks` past the floor and
/// `pending_ticks` only counts video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaTime {
    /// The plan entry's start, in video ticks from t=0.
    start_ticks: u64,
    /// Video ticks to add to every `tfdt`. Learned from the first fragment
    /// that carries video; `None` until one arrives.
    delta: Option<i64>,
}

/// The boundaries a plan already decided, in film time.
///
/// `starts[0]` is where `first_index` begins, `starts[1]` where the next entry
/// begins, and so on. Only video entries appear: an audio tail has no video
/// boundary to cut on, and [`Segmenter::finish`] splits it by the same rule
/// the plan used.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Boundaries {
    first_index: u64,
    starts: Vec<u64>,
}

/// Cuts a verified, film-global AAC sample stream on its own immutable plan.
/// Callers remove encoder priming and restore absolute `tfdt` before pushing.
/// A video boundary never participates in this soundtrack's publication.
#[derive(Debug)]
pub struct PlannedAudioSegmenter {
    init: Init,
    plan: crate::segplan::SegmentPlan,
    entry: usize,
    next_tick: u64,
    pending: Vec<Fragment>,
    pending_bytes: usize,
}

impl PlannedAudioSegmenter {
    pub fn new(
        init: Init,
        plan: crate::segplan::SegmentPlan,
        start_entry: u32,
    ) -> Result<Self, Fmp4Error> {
        let refuse = || {
            Fmp4Error::Unsupported(
                "shared AAC requires one 48kHz track and a contiguous sample-clock plan".into(),
            )
        };
        if init.tracks.len() != 1
            || init.tracks[0].kind != TrackKind::Audio
            || init.tracks[0].timescale != 48_000
            || plan.timescale != 48_000
            || plan.version != crate::segplan::SEGPLAN_VERSION
            || plan.entries.is_empty()
            || plan.entries.len() > 100_000
        {
            return Err(refuse());
        }
        let mut next = 0;
        for (index, entry) in plan.entries.iter().enumerate() {
            let final_entry = index + 1 == plan.entries.len();
            if entry.index as usize != index
                || entry.kind != crate::segplan::PlanEntryKind::AudioTail
                || entry.start_ticks != next
                || entry.start_ticks % 1_024 != 0
                || entry.duration_ticks == 0
                || entry.duration_ticks > 480_000
                || (!final_entry && entry.duration_ticks % 1_024 != 0)
            {
                return Err(refuse());
            }
            next = next.checked_add(entry.duration_ticks).ok_or_else(refuse)?;
        }
        let next_tick = plan.entry(start_entry).ok_or_else(refuse)?.start_ticks;
        Ok(Self {
            init,
            plan,
            entry: start_entry as usize,
            next_tick,
            pending: Vec::new(),
            pending_bytes: 0,
        })
    }

    /// Fully validate the incoming sample run before changing publication
    /// state. Gaps and overlaps are refused rather than absorbed into an AAC
    /// sample duration by the general-purpose fragment merger.
    pub fn push(&mut self, fragment: Fragment) -> Result<Vec<Published>, Fmp4Error> {
        let refuse =
            || Fmp4Error::Malformed("shared AAC sample bounds or clock continuity failed".into());
        if fragment.tracks.len() != 1
            || fragment.len() > 4 * 1024 * 1024
            || fragment.tracks[0].track_id != self.init.tracks[0].id
            || fragment.tracks[0].base_decode_time != self.next_tick
            || fragment.tracks[0].sample_count() == 0
            || fragment.tracks[0].sample_count() > 2_048
        {
            return Err(refuse());
        }
        let end = self
            .plan
            .entries
            .last()
            .expect("validated plan")
            .end_ticks();
        let track = &fragment.tracks[0];
        let mut tick = self.next_tick;
        let mut planned_entry = self.entry;
        let mut interval_bytes = self.pending_bytes;
        let mut payloads = Vec::with_capacity(track.sample_count());
        for run in &track.runs {
            let mut offset = run.data_offset;
            for sample in &run.samples {
                let through = offset
                    .checked_add(sample.size as usize)
                    .ok_or_else(refuse)?;
                if sample.duration == 0
                    || sample.duration > 1_024
                    || sample.size == 0
                    || sample.cto != 0
                    || offset < fragment.mdat_payload.start
                    || through > fragment.mdat_payload.end
                    || through > fragment.bytes.len()
                    || (sample.duration != 1_024 && tick + u64::from(sample.duration) != end)
                {
                    return Err(refuse());
                }
                let next = tick
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(refuse)?;
                if next > end {
                    return Err(refuse());
                }
                let entry = self.plan.entries.get(planned_entry).ok_or_else(refuse)?;
                if next > entry.end_ticks() {
                    return Err(refuse());
                }
                interval_bytes = interval_bytes
                    .checked_add(sample.size as usize)
                    .ok_or_else(refuse)?;
                if interval_bytes > 16 * 1024 * 1024 {
                    return Err(Fmp4Error::Unsupported(
                        "shared AAC interval exceeds its publication byte allowance".into(),
                    ));
                }
                if next == entry.end_ticks() {
                    planned_entry += 1;
                    interval_bytes = 0;
                }
                payloads.push((tick, *sample, fragment.bytes[offset..through].to_vec()));
                tick = next;
                offset = through;
            }
        }
        let mut published = Vec::new();
        for (tick, mut sample, bytes) in payloads {
            let entry = self.plan.entries.get(self.entry).ok_or_else(refuse)?;
            let next = tick + u64::from(sample.duration);
            if next > entry.end_ticks() {
                return Err(refuse());
            }
            self.pending_bytes += bytes.len();
            if self.pending_bytes > 16 * 1024 * 1024 {
                return Err(Fmp4Error::Unsupported(
                    "shared AAC interval exceeds its publication byte allowance".into(),
                ));
            }
            sample.size_at = None;
            self.pending.push(Fragment {
                mdat_payload: 0..bytes.len(),
                bytes,
                tracks: vec![TrackFragment {
                    track_id: self.init.tracks[0].id,
                    base_decode_time: tick,
                    runs: vec![Run {
                        version: 0,
                        composition_offsets_present: false,
                        data_offset: 0,
                        data_offset_at: None,
                        samples: vec![sample],
                    }],
                }],
            });
            self.next_tick = next;
            if next == entry.end_ticks() {
                let segment = merge(&self.pending, &self.init, entry.index + 1)?;
                if segment.stats.tfdt_adjustments != 0 {
                    return Err(refuse());
                }
                published.push(Published {
                    index: u64::from(entry.index),
                    segment,
                    reason: if self.entry + 1 == self.plan.entries.len() {
                        CutReason::EndOfStream
                    } else {
                        CutReason::Clean
                    },
                    seconds: entry.duration_ticks as f64 / 48_000.0,
                });
                self.entry += 1;
                self.pending.clear();
                self.pending_bytes = 0;
            }
        }
        Ok(published)
    }

    /// A killed or truncated producer cannot publish a partial promised URI.
    pub fn complete(&self) -> bool {
        self.entry == self.plan.entries.len() && self.pending.is_empty()
    }
}

/// Accumulates fragments and publishes segments that start on clean keyframes.
///
/// Pure by design: the daemon feeds it fragments and writes what comes back
/// out, so every rule about where a boundary may land is testable without a
/// filesystem, a process or a clock.
#[derive(Debug)]
pub struct Segmenter {
    init: Init,
    policy: CutPolicy,
    video_timescale: u32,
    pending: Vec<Fragment>,
    pending_bytes: usize,
    pending_ticks: u64,
    next_index: u64,
    media_time: Option<MediaTime>,
    boundaries: Option<Boundaries>,
    encoded_audio_boundaries: bool,
    counts: SegmentCounts,
    retain_hevc_parameter_sets: bool,
}

impl Segmenter {
    pub fn new(init: Init, policy: CutPolicy) -> Segmenter {
        let video_timescale = init.video().map(|v| v.timescale).unwrap_or(0).max(1);
        Segmenter {
            init,
            policy,
            video_timescale,
            pending: Vec::new(),
            pending_bytes: 0,
            pending_ticks: 0,
            next_index: 0,
            media_time: None,
            boundaries: None,
            encoded_audio_boundaries: false,
            counts: SegmentCounts::default(),
            retain_hevc_parameter_sets: false,
        }
    }

    /// Keep every video sample's in-band HEVC parameter sets when merging.
    ///
    /// Set from the copy's own `CopyVideoOptions::retains_hevc_parameter_sets`,
    /// so a segmenter can never delete what the copy's bitstream filter was
    /// built to keep. See [`merge_retaining`].
    pub fn retaining_hevc_parameter_sets(mut self, retain: bool) -> Segmenter {
        self.retain_hevc_parameter_sets = retain;
        self
    }

    /// A segmenter that cuts where a plan already decided, rather than
    /// deciding again.
    ///
    /// `starts` is the film time, in video ticks, at which each of this
    /// generation's segments begins, in order, starting with `start_index` —
    /// exactly `PlanEntry::start_ticks` for the video entries from
    /// `start_index` onward. `starts[0]` is also where the generation is
    /// rebased to, so a caller cannot supply an index and a time that
    /// disagree.
    ///
    /// This is what ledger D10 means by the plan being normative. Every
    /// boundary here is one a client already has in a playlist it fetched, and
    /// a generation that re-derived them would eventually pick a different
    /// one — see [`Segmenter::cut_before`] for why the two derivations cannot
    /// agree. The [`CutPolicy`] is still carried, because
    /// [`Segmenter::finish`] uses its ceiling to split an audio tail, but it
    /// no longer chooses a video boundary.
    ///
    /// Refused for an empty `starts`, and for an init with no video track, for
    /// the same reason [`Segmenter::resuming_at`] is.
    pub fn following(
        init: Init,
        policy: CutPolicy,
        start_index: u64,
        starts: Vec<u64>,
    ) -> Result<Segmenter, Fmp4Error> {
        let Some(&first) = starts.first() else {
            return Err(Fmp4Error::Unsupported(
                "a planned generation needs at least one boundary".into(),
            ));
        };
        let mut segmenter = Segmenter::resuming_at(init, policy, start_index, first)?;
        segmenter.boundaries = Some(Boundaries {
            first_index: start_index,
            starts,
        });
        Ok(segmenter)
    }

    /// Encoded AAC belongs to the declared presentation intervals, not the
    /// muxer's decode-order fragment boundaries. With B frames those boundaries
    /// precede the next picture by the reorder delay; restarting at that picture
    /// otherwise drops the AAC packets a continuous generation assigned to it.
    pub fn following_encoded(
        init: Init,
        policy: CutPolicy,
        start_index: u64,
        starts: Vec<u64>,
    ) -> Result<Segmenter, Fmp4Error> {
        let mut segmenter = Self::following(init, policy, start_index, starts)?;
        segmenter.encoded_audio_boundaries = true;
        Ok(segmenter)
    }

    /// A segmenter for a generation that starts partway into the film.
    ///
    /// `start_index` is the plan index of the first segment this generation
    /// will publish, and `start_ticks` is that entry's start in video ticks
    /// from t=0. Both come from the plan, never from the media: the whole
    /// point is that this generation's own timestamps cannot be trusted to say
    /// where in the film it is.
    ///
    /// Two things follow from the index, and both matter. The segment gets its
    /// real name rather than `seg00000`, and the first-segment floor — which
    /// exists to get a *session* started quickly — is not spent again on a
    /// segment that is nobody's first.
    ///
    /// Refused for an init with no video track, because there is then nothing
    /// to anchor a film time against and every alternative is a guess: an
    /// audio clock says where in *its own* stream a sample is, never where in
    /// the film. A repositioned audio-only rendition is not supported, and
    /// saying so is better than placing one wherever the guess landed. A
    /// video-bearing init reaches this refusal never.
    pub fn resuming_at(
        init: Init,
        policy: CutPolicy,
        start_index: u64,
        start_ticks: u64,
    ) -> Result<Segmenter, Fmp4Error> {
        if init.video().is_none() {
            return Err(Fmp4Error::Unsupported(
                "a repositioned generation needs a video track to anchor film \
                 time against; this init declares none"
                    .into(),
            ));
        }
        let mut segmenter = Segmenter::new(init, policy);
        segmenter.next_index = start_index;
        segmenter.media_time = Some(MediaTime {
            start_ticks,
            delta: None,
        });
        Ok(segmenter)
    }

    pub fn init(&self) -> &Init {
        &self.init
    }

    pub fn counts(&self) -> SegmentCounts {
        self.counts
    }

    /// How long the pending run runs, in seconds — the number that becomes
    /// `EXTINF`.
    ///
    /// The video track's summed sample durations in the video timescale, per
    /// STUTTER-4K §6.3: audio fragment durations differ from video's by up to
    /// one AAC frame, and using them drifts the playlist against the media
    /// over a film.
    ///
    /// At end of stream the longest track is authoritative. ffmpeg may put the
    /// last video frame and a much longer audio tail in the same fragment, so
    /// falling back only when video duration is exactly zero truncates that
    /// mixed final segment in the playlist. Ordinary cuts still use video: an
    /// audio frame of per-fragment drift must not accumulate across the film.
    fn seconds_for(&self, fragments: &[Fragment], reason: CutReason) -> f64 {
        let video_ticks: u64 = self
            .init
            .video()
            .map(|video| {
                fragments
                    .iter()
                    .filter_map(|fragment| fragment.track(video.id))
                    .map(TrackFragment::duration)
                    .sum()
            })
            .unwrap_or(0);
        let video_seconds = video_ticks as f64 / self.video_timescale as f64;
        if video_ticks > 0 && reason != CutReason::EndOfStream {
            return video_seconds;
        }
        let mut longest = video_seconds;
        for track in &self.init.tracks {
            let ticks: u64 = fragments
                .iter()
                .filter_map(|f| f.track(track.id))
                .map(|t| t.duration())
                .sum();
            let secs = ticks as f64 / track.timescale.max(1) as f64;
            if secs > longest {
                longest = secs;
            }
        }
        longest
    }

    /// Bytes held back waiting for a cut point. Bounded by the byte ceiling
    /// plus one fragment; anything beyond that is worth a log line.
    pub fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// Feed the next fragment. Returns a segment when this fragment's arrival
    /// is what ended the previous one — the cut is decided *in front of* the
    /// new fragment, which is the whole trick: a boundary can only be judged
    /// once you can see what comes after it.
    pub fn push(&mut self, fragment: Fragment) -> Result<Option<Published>, Fmp4Error> {
        let mut fragment = fragment;
        self.on_film_time(&mut fragment)?;
        let class = classify(&fragment, &self.init);
        self.counts.fragments += 1;
        if class == CutClass::Unparseable {
            self.counts.unparseable += 1;
        }
        let published = match self.cut_before(&fragment, class)? {
            Some(reason) => {
                if self.encoded_audio_boundaries {
                    let plan = self.boundaries.as_ref().expect("encoded plan");
                    let boundary = plan.starts[self.entry_after(plan)];
                    if let Some(prefix) = take_encoded_audio_before(
                        &mut fragment,
                        &self.init,
                        boundary,
                        self.video_timescale,
                    )? {
                        self.pending_bytes += prefix.len();
                        self.pending.push(prefix);
                    }
                }
                Some(self.flush(reason)?)
            }
            None => None,
        };
        self.pending_ticks += fragment.video_duration(&self.init);
        self.pending_bytes += fragment.len();
        self.pending.push(fragment);
        Ok(published)
    }

    /// Should the pending run be published in front of this fragment?
    ///
    /// Two answers, and which one applies is the difference between a session
    /// and a rendition.
    ///
    /// **With a plan**, the boundaries were decided once, are persisted, and
    /// are already in a playlist the client holds — ledger D10 makes the plan
    /// normative, so this is a lookup and not a decision. Re-running the
    /// policy here would not reproduce it: [`crate::segplan::plan_copy`] runs
    /// the same policy over the *video-only* index pipe's wire lengths plus a
    /// deliberately generous audio estimate, while this segmenter accumulates
    /// the real production wire length. Those agree on a clean cut, where the
    /// keyframe decides, and disagree on a byte-ceiling cut, where the bytes
    /// do — which on a 69 Mb/s remux with no clean point in reach is the
    /// ordinary case, not an edge. Every index after such a disagreement would
    /// name the wrong plan entry, silently.
    ///
    /// **Without one**, this is a live session cutting as it goes, and the
    /// policy is the whole answer — unchanged.
    fn cut_before(
        &self,
        fragment: &Fragment,
        class: CutClass,
    ) -> Result<Option<CutReason>, Fmp4Error> {
        let Some(plan) = self.boundaries.as_ref() else {
            if let Some(strict) = self.policy.strict_max_ticks {
                let incoming = fragment.video_duration(&self.init);
                if incoming > strict {
                    return Err(Fmp4Error::Unsupported(format!(
                        "one video fragment runs {:.3}s, longer than the fixed {:.3}s rolling HLS target",
                        incoming as f64 / self.video_timescale as f64,
                        strict as f64 / self.video_timescale as f64,
                    )));
                }
                if !self.pending.is_empty() && self.pending_ticks.saturating_add(incoming) > strict
                {
                    return Ok(Some(CutReason::TimeCeiling));
                }
            }
            return Ok(self.policy.cut_before(
                self.pending_ticks,
                self.pending_bytes,
                class,
                self.next_index == 0,
            ));
        };
        if self.pending.is_empty() {
            return Ok(None);
        }
        let Some(next) = plan.starts.get(self.entry_after(plan)) else {
            // Past the last planned boundary. Everything left belongs to the
            // final entry, and `finish` publishes it.
            return Ok(None);
        };
        // A fragment carrying no video says nothing about where a video
        // boundary is. It joins whatever run is open.
        let Some(dts) = self.film_time_of(fragment) else {
            return Ok(None);
        };
        if dts < *next {
            return Ok(None);
        }
        // A fragment that reaches past *two* boundaries would leave an entry
        // with no media in it, and publishing an empty segment under an index
        // a client will request is worse than refusing to produce it. It means
        // this generation is not the one that was planned.
        if let Some(after) = plan.starts.get(self.entry_after(plan) + 1) {
            if dts >= *after {
                return Err(Fmp4Error::Unsupported(format!(
                    "a fragment at film time {dts} steps over the planned \
                     boundaries at {next} and {after}; this generation does \
                     not match the plan that named it"
                )));
            }
        }
        Ok(Some(if class.is_clean() {
            CutReason::Clean
        } else {
            // The plan already recorded why it cut here; what this segmenter
            // knows is only whether the fragment it is cutting in front of is
            // a clean point. Saying `TimeCeiling` for anything else would
            // slander a boundary the byte ceiling chose, so the counters treat
            // both the same way and only `clean_cuts` is a claim.
            CutReason::TimeCeiling
        }))
    }

    /// The plan entry this segmenter is about to finish, as an offset into the
    /// plan's boundary list.
    fn entry_after(&self, plan: &Boundaries) -> usize {
        (self.next_index - plan.first_index) as usize + 1
    }

    /// A fragment's video decode time, on the film's timeline.
    ///
    /// After [`Segmenter::on_film_time`] the fragment already carries it; this
    /// only picks the video track out.
    fn film_time_of(&self, fragment: &Fragment) -> Option<u64> {
        self.init
            .video()
            .and_then(|video| fragment.track(video.id))
            .map(|track| track.base_decode_time)
    }

    /// End of stream: whatever is still pending becomes the final segment(s).
    ///
    /// A muxed fragment can contain the last video frame followed by a much
    /// longer audio tail. Publishing that whole fragment as one HLS segment
    /// makes its truthful `EXTINF` arbitrarily large and raises the playlist's
    /// reload interval with it. Split only at audio sample boundaries here:
    /// video remains intact, every encoded sample is copied verbatim, and the
    /// tail stays within the same duration ceiling as the rest of the stream.
    pub fn finish(&mut self) -> Result<Vec<Published>, Fmp4Error> {
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = self.end_of_stream_chunks()?;
        let mut published = Vec::with_capacity(chunks.len());
        for (offset, fragments) in chunks.iter().enumerate() {
            let index = self.next_index + offset as u64;
            let seconds = self.seconds_for(fragments, CutReason::EndOfStream);
            if let Some(strict_ticks) = self.policy.strict_max_ticks {
                let strict_seconds = strict_ticks as f64 / self.video_timescale as f64;
                if !seconds.is_finite() || seconds <= 0.0 || seconds > strict_seconds {
                    return Err(Fmp4Error::Unsupported(format!(
                        "final segment {index} has a truthful EXTINF of {seconds:.6}s, outside the fixed {strict_seconds:.3}s rolling HLS target"
                    )));
                }
            }
            let segment = merge_retaining(
                fragments,
                &self.init,
                index as u32 + 1,
                self.retain_hevc_parameter_sets,
            )?;
            published.push(Published {
                index,
                segment,
                reason: CutReason::EndOfStream,
                seconds,
            });
        }

        self.pending.clear();
        self.pending_bytes = 0;
        self.pending_ticks = 0;
        self.next_index += published.len() as u64;
        self.counts.segments += published.len() as u64;
        self.counts.tfdt_adjustments += published
            .iter()
            .map(|item| item.segment.stats.tfdt_adjustments)
            .sum::<u32>();
        Ok(published)
    }

    /// Partition the pending media without rewriting a sample. The first
    /// chunk is allowed to be as long as the unsplittable video run; later
    /// chunks contain only trailing tracks and observe the policy ceiling.
    fn end_of_stream_chunks(&self) -> Result<Vec<Vec<Fragment>>, Fmp4Error> {
        let video_ticks: u64 = self
            .pending
            .iter()
            .map(|fragment| fragment.video_duration(&self.init))
            .sum();
        let mut chunks: Vec<Vec<Fragment>> = Vec::new();

        for track in &self.init.tracks {
            // `max_ticks` is the cutting ceiling; `strict_max_ticks` is the
            // fixed covering duration advertised to the client. Keep the
            // two separate here. Audio samples are indivisible, so allowing
            // one boundary sample past an already-covering target can make a
            // 16 second target produce a truthful 16.014 second EXTINF and
            // then reject itself in `finish`.
            let ceiling = scale_ticks(
                self.policy.max_ticks,
                self.video_timescale,
                track.timescale.max(1),
            )
            .max(1);
            let video_limit =
                scale_ticks(video_ticks, self.video_timescale, track.timescale.max(1));
            // How much audio the FINAL VIDEO segment is allowed to absorb, and
            // the one place the planned and unplanned paths part company.
            //
            // Without a plan this is `ceiling.max(video_limit)`: the last video
            // segment may keep audio out to the whole duration ceiling, which
            // is the `c58a4307` rule and is what a live session wants — fewer
            // boundaries, and nobody is holding a playlist that says otherwise.
            //
            // With a plan it is `video_limit` exactly. `segplan`'s
            // `append_audio_tail` models the tail as beginning where video
            // ends and splitting at the ceiling from there, so a segmenter
            // that let the last video segment swallow up to a ceiling of audio
            // produces a different number of segments than the plan named —
            // and the plan is already in a playlist the client holds, declared
            // VOD and closed with ENDLIST. The reader then blocks on a segment
            // no producer will ever emit and burns its first-byte deadline at
            // the end of every affected film. That is ledger D10 again: the
            // plan decides, here as everywhere.
            let first_limit = if self.boundaries.is_some() {
                video_limit.max(1)
            } else {
                ceiling.max(video_limit)
            };
            let allow_boundary_sample = video_ticks > 0;
            let mut chunk_index = 0usize;
            let mut chunk_ticks = 0u64;

            for fragment in &self.pending {
                let Some(track_fragment) = fragment.track(track.id) else {
                    continue;
                };
                let mut decode_time = track_fragment.base_decode_time;
                for run in &track_fragment.runs {
                    let mut data_offset = run.data_offset;
                    let mut group_start = 0usize;
                    while group_start < run.samples.len() {
                        let sample = run.samples[group_start];
                        let limit = if chunk_index == 0 {
                            first_limit
                        } else {
                            ceiling
                        };
                        if sample.duration as u64 > ceiling {
                            return Err(Fmp4Error::Unsupported(format!(
                                "track {} has a {:.3}s sample longer than the {:.3}s HLS ceiling",
                                track.id,
                                sample.duration as f64 / track.timescale.max(1) as f64,
                                ceiling as f64 / track.timescale.max(1) as f64,
                            )));
                        }
                        let crosses_limit = chunk_ticks > 0
                            && chunk_ticks.saturating_add(sample.duration as u64) > limit;
                        let starts_after_video_boundary =
                            chunk_index == 0 && allow_boundary_sample && chunk_ticks >= limit;
                        if starts_after_video_boundary
                            || (crosses_limit && !(chunk_index == 0 && allow_boundary_sample))
                        {
                            chunk_index += 1;
                            chunk_ticks = 0;
                            continue;
                        }

                        let selected_chunk = chunk_index;
                        let selected_decode_time = decode_time;
                        let selected_data_offset = data_offset;
                        let mut group_end = group_start;
                        while group_end < run.samples.len() {
                            let candidate = run.samples[group_end];
                            if candidate.duration as u64 > ceiling {
                                return Err(Fmp4Error::Unsupported(format!(
                                    "track {} has a {:.3}s sample longer than the {:.3}s HLS ceiling",
                                    track.id,
                                    candidate.duration as f64 / track.timescale.max(1) as f64,
                                    ceiling as f64 / track.timescale.max(1) as f64,
                                )));
                            }
                            let limit = if chunk_index == 0 {
                                first_limit
                            } else {
                                ceiling
                            };
                            let crosses_limit = chunk_ticks > 0
                                && chunk_ticks.saturating_add(candidate.duration as u64) > limit;
                            let starts_after_video_boundary =
                                chunk_index == 0 && allow_boundary_sample && chunk_ticks >= limit;
                            if starts_after_video_boundary
                                || (crosses_limit && !(chunk_index == 0 && allow_boundary_sample))
                            {
                                break;
                            }
                            chunk_ticks += candidate.duration as u64;
                            decode_time += candidate.duration as u64;
                            data_offset += candidate.size as usize;
                            group_end += 1;
                        }

                        if chunks.len() <= selected_chunk {
                            chunks.resize_with(selected_chunk + 1, Vec::new);
                        }
                        chunks[selected_chunk].push(Fragment {
                            bytes: fragment.bytes.clone(),
                            mdat_payload: fragment.mdat_payload.clone(),
                            tracks: vec![TrackFragment {
                                track_id: track.id,
                                base_decode_time: selected_decode_time,
                                runs: vec![Run {
                                    version: run.version,
                                    composition_offsets_present: run.composition_offsets_present,
                                    data_offset_at: None,
                                    data_offset: selected_data_offset,
                                    samples: run.samples[group_start..group_end].to_vec(),
                                }],
                            }],
                        });
                        group_start = group_end;
                    }
                }
            }
        }

        chunks.retain(|chunk| !chunk.is_empty());
        if chunks.is_empty() {
            return malformed("no track survived the end-of-stream split");
        }
        Ok(chunks)
    }

    /// Put a repositioned generation's fragments back on the film's timeline.
    ///
    /// Hands `fragment` straight back when this generation started at t = 0
    /// and is already on that timeline.
    ///
    /// The shift is computed once, in video ticks, and converted into each
    /// track's own clock. Every track keeps its offset from video, because the
    /// shift is added to what is already there rather than substituted for it.
    /// It is not preserved *exactly*: [`scale_signed`] rounds, so a track
    /// whose timescale differs from video's moves by under one of its own
    /// ticks. Once, for the generation, not per fragment.
    ///
    /// Until a fragment carrying video arrives there is nothing to anchor
    /// against, so nothing is shifted — an audio clock says where in its own
    /// stream a sample is, never where in the film. When the anchor does
    /// arrive, everything already accumulated is shifted with it, which is why
    /// this takes `&mut self` rather than returning a fragment: leaving the
    /// leading audio-only fragments on ffmpeg's clock inside a segment whose
    /// video is on the film's is a multi-second A/V split that [`merge`]'s
    /// discontinuity repair would partly absorb and partly not count.
    fn on_film_time(&mut self, fragment: &mut Fragment) -> Result<(), Fmp4Error> {
        let Some(media) = self.media_time else {
            return Ok(());
        };
        let learned = match media.delta {
            Some(delta) => delta,
            None => {
                let video = self
                    .init
                    .video()
                    .ok_or_else(|| Fmp4Error::Unsupported("no video track to anchor".into()))?;
                let Some(base) = fragment.track(video.id).map(|t| t.base_decode_time) else {
                    // No anchor yet. This fragment waits with the rest.
                    return Ok(());
                };
                // In i128 because a `tfdt` is a 64-bit field read straight off
                // the wire: a source whose decode times sit above 2^63 would
                // otherwise overflow the subtraction and place the whole
                // generation at a time of the wrong sign.
                let delta = i128::from(media.start_ticks) - i128::from(base);
                let delta = i64::try_from(delta).map_err(|_| {
                    Fmp4Error::Unsupported(format!(
                        "this generation's first decode time is {base}, which is \
                         {} ticks from the plan's {} — too far to place",
                        delta.unsigned_abs(),
                        media.start_ticks
                    ))
                })?;
                self.refuse_if_any_track_lands_before_zero(fragment, delta)?;
                if let Some(media) = self.media_time.as_mut() {
                    media.delta = Some(delta);
                }
                // Everything that arrived before the anchor moves with it.
                let mut pending = std::mem::take(&mut self.pending);
                for earlier in &mut pending {
                    self.shift(earlier, delta);
                }
                self.pending = pending;
                delta
            }
        };
        self.shift(fragment, learned);
        Ok(())
    }

    /// Add one generation's shift to every track of one fragment.
    fn shift(&self, fragment: &mut Fragment, delta: i64) {
        for track in &mut fragment.tracks {
            let shift = scale_signed(
                delta,
                self.video_timescale,
                self.timescale_of(track.track_id),
            );
            track.base_decode_time = track.base_decode_time.saturating_add_signed(shift);
        }
    }

    fn timescale_of(&self, track_id: u32) -> u32 {
        self.init
            .tracks
            .iter()
            .find(|candidate| candidate.id == track_id)
            .map(|candidate| candidate.timescale.max(1))
            .unwrap_or(self.video_timescale)
    }

    /// A shift that would take any track below zero is refused, not clamped.
    ///
    /// `-avoid_negative_ts make_zero` puts the *earliest* track at zero, which
    /// on a muxed stream is usually audio, so video's first decode time is
    /// already some way above it. Rebasing video onto a small `start_ticks`
    /// therefore asks audio to start before the film does, and there is no
    /// such time to write: `tfdt` is unsigned. Saturating at zero would answer
    /// by silently moving audio away from picture by exactly the amount that
    /// did not fit — the one thing this whole function exists to prevent — and
    /// [`merge`]'s repair would not always count it, because a negative join
    /// leaves the range it checks. So the generation is refused and the caller
    /// is told which track could not be placed.
    fn refuse_if_any_track_lands_before_zero(
        &self,
        fragment: &Fragment,
        delta: i64,
    ) -> Result<(), Fmp4Error> {
        for track in &fragment.tracks {
            let shift = scale_signed(
                delta,
                self.video_timescale,
                self.timescale_of(track.track_id),
            );
            if i128::from(track.base_decode_time) + i128::from(shift) < 0 {
                return Err(Fmp4Error::Unsupported(format!(
                    "track {} starts {} ticks before the film time this \
                     generation was placed at, and a decode time cannot be \
                     negative",
                    track.track_id,
                    -(i128::from(track.base_decode_time) + i128::from(shift)),
                )));
            }
        }
        Ok(())
    }

    fn flush(&mut self, reason: CutReason) -> Result<Published, Fmp4Error> {
        let index = self.next_index;
        let seconds = self.seconds_for(&self.pending, reason);
        let segment = merge_retaining(
            &self.pending,
            &self.init,
            index as u32 + 1,
            self.retain_hevc_parameter_sets,
        )?;
        self.pending.clear();
        self.pending_bytes = 0;
        self.pending_ticks = 0;
        self.next_index += 1;
        self.counts.segments += 1;
        self.counts.tfdt_adjustments += segment.stats.tfdt_adjustments;
        match reason {
            CutReason::Clean => self.counts.clean_cuts += 1,
            CutReason::ByteCeiling | CutReason::TimeCeiling => self.counts.ceiling_cuts += 1,
            // The last segment of a session did not choose where it ended, so
            // it is neither a clean cut nor a ceiling cut. Counting it as
            // either would flatter or slander the policy.
            CutReason::EndOfStream => {}
        }
        Ok(Published {
            index,
            segment,
            reason,
            seconds,
        })
    }
}

/// Move complete AAC packets whose start precedes the next presentation
/// boundary into the preceding segment. Copy only those packet payloads;
/// video and the rest of the muxed fragment remain in their existing buffer.
/// The merger writes fresh moof/data offsets from these resolved track runs.
fn take_encoded_audio_before(
    fragment: &mut Fragment,
    init: &Init,
    boundary: u64,
    video_timescale: u32,
) -> Result<Option<Fragment>, Fmp4Error> {
    let mut bytes = Vec::new();
    let mut tracks = Vec::new();
    for track in &mut fragment.tracks {
        let Some(info) = init
            .tracks
            .iter()
            .find(|info| info.id == track.track_id && info.kind == TrackKind::Audio)
        else {
            continue;
        };
        let cutoff = u128::from(boundary) * u128::from(info.timescale);
        let mut time = track.base_decode_time;
        let base_decode_time = time;
        let mut runs = Vec::new();
        for run in &mut track.runs {
            let mut count = 0;
            let mut length = 0usize;
            for sample in &run.samples {
                if u128::from(time) * u128::from(video_timescale) >= cutoff {
                    break;
                }
                time = time
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(|| Fmp4Error::Malformed("encoded audio time overflow".into()))?;
                length = length
                    .checked_add(sample.size as usize)
                    .ok_or_else(|| Fmp4Error::Malformed("encoded audio size overflow".into()))?;
                count += 1;
            }
            if count == 0 {
                break;
            }
            let end = run
                .data_offset
                .checked_add(length)
                .ok_or_else(|| Fmp4Error::Malformed("encoded audio offset overflow".into()))?;
            let payload = fragment
                .bytes
                .get(run.data_offset..end)
                .ok_or_else(|| Fmp4Error::Malformed("encoded audio leaves its payload".into()))?;
            let data_offset = bytes.len();
            bytes.extend_from_slice(payload);
            let samples = run
                .samples
                .drain(..count)
                .map(|mut sample| {
                    sample.size_at = None;
                    sample
                })
                .collect();
            runs.push(Run {
                version: run.version,
                composition_offsets_present: run.composition_offsets_present,
                data_offset_at: None,
                data_offset,
                samples,
            });
            run.data_offset = end;
            run.data_offset_at = None;
            if !run.samples.is_empty() {
                break;
            }
        }
        if !runs.is_empty() {
            track.base_decode_time = time;
            track.runs.retain(|run| !run.samples.is_empty());
            tracks.push(TrackFragment {
                track_id: track.track_id,
                base_decode_time,
                runs,
            });
        }
    }
    fragment.tracks.retain(|track| track.sample_count() > 0);
    Ok((!tracks.is_empty()).then_some(Fragment {
        mdat_payload: 0..bytes.len(),
        bytes,
        tracks,
    }))
}

// ---------------------------------------------------------------------------
// playlist
// ---------------------------------------------------------------------------

/// The playlist header, rewritten with the playlist on every segment.
///
/// The caller supplies a covering target that was fixed before the first
/// response. Rolling writers must enforce that bound before publishing every
/// segment; immutable VOD supplies the covering target from its completed
/// plan. A served target must never grow under a client that already holds an
/// earlier snapshot.
///
/// No `#EXT-X-INDEPENDENT-SEGMENTS`. With clean cuts the claim would actually
/// be true for most segments — but a ceiling cut makes it a lie again, and a
/// lie in a spec tag is what this whole path is here to stop shipping.
pub fn playlist_header(target_duration: u32) -> String {
    format!(
        "#EXTM3U\n\
         #EXT-X-VERSION:7\n\
         #EXT-X-TARGETDURATION:{target_duration}\n\
         #EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:EVENT\n\
         #EXT-X-MAP:URI=\"init.mp4\"\n"
    )
}

/// One published segment's two lines. Six decimals, from summed video sample
/// durations in the video timescale — never wall clock, never audio.
pub fn playlist_entry(seconds: f64, name: &str) -> String {
    let mut s = String::new();
    let _ = write!(s, "#EXTINF:{seconds:.6},\n{name}\n");
    s
}

/// `seg00000.m4s` — five digits from zero, exactly what `-start_number 0` and
/// `seg%05d.m4s` gave the muxer this replaces. The daemon's segment index
/// parses these names.
pub fn segment_name(index: u64) -> String {
    format!("seg{index:05}.m4s")
}

#[cfg(test)]
mod tests;
