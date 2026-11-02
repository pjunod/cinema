//! Sparse reads of Matroska subtitle tracks.
//!
//! An embedded subtitle track is a few hundred kilobytes interleaved through
//! a container that can be 50–90 GB. Demuxing it with FFmpeg reads every
//! video and audio byte of the span it covers, so the cost of extracting a
//! subtitle was set by the picture's bitrate, not by the subtitles: a
//! whole-track extraction of a 4K remux over the NAS ran for 25 minutes and
//! more (and hit the 30-minute source-pass deadline, to be retried from zero
//! on another node), and one 200-second playback window read about 2.5 GB
//! and took 25 to 186 seconds — longer, at the slow end, than the window it
//! was bridging.
//!
//! Matroska already indexes subtitle blocks. mkvmerge writes a cue point for
//! every subtitle frame and records each track's frame count in its
//! statistics tags. When a track's distinct cue references are exactly as
//! many as its `NUMBER_OF_FRAMES` statistic, and every reference resolves to
//! a block of that track, the cues name every block of the track. The track
//! can then be read with two small reads per block instead of a demux of the
//! whole file.
//!
//! The blocks are not interpreted here. They are copied verbatim, together
//! with the original EBML header, `Info` and every subtitle `TrackEntry`,
//! into a subtitle-only Matroska file, and the existing FFmpeg command runs
//! against that file. The demuxer, codec private data, content encodings and
//! WebVTT conversion are therefore the ones every other path uses, and the
//! subtitle ordinals (`0:s:N`) are unchanged because every subtitle track
//! entry is kept in its original order.
//!
//! Anything that does not meet that bar is [`SparseError::NotIndexed`] and
//! the caller keeps its full demux: other containers, tracks without
//! per-frame cues, laced blocks, missing or stale statistics, and track lists
//! that disagree with the library scan.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

const HEAD_BYTES: usize = 64 * 1024;
const MAX_METADATA: u64 = 32 * 1024 * 1024;
const MAX_BLOCK: u64 = 8 * 1024 * 1024;
/// Text tracks are kilobytes and a film's PGS track tens of megabytes; past
/// this the subset is not the cheap answer it exists to be.
const MAX_SUBSET_BYTES: usize = 128 * 1024 * 1024;
const MAX_REFERENCES: usize = 500_000;
const MAX_TOP_LEVEL_WALK: usize = 64;
const ELEMENT_HEADER_READ: usize = 16;
const CLUSTER_HEADER_READ: usize = 64;
const BLOCK_READ: usize = 4096;
/// Concurrent positioned reads. A spinning-disk NAS under load answers one
/// random read in ~20 ms, so a serial walk over a thousand cues spends a
/// minute waiting; a handful of outstanding reads keeps the array busy
/// without competing with playback for more than a sliver of its bandwidth.
const READERS: usize = 8;

const EBML: u32 = 0x1A45_DFA3;
const DOC_TYPE: u32 = 0x4282;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const TAGS: u32 = 0x1254_C367;
const TAG: u32 = 0x7373;
const TARGETS: u32 = 0x63C0;
const TAG_TRACK_UID: u32 = 0x63C5;
const SIMPLE_TAG: u32 = 0x67C8;
const TAG_NAME: u32 = 0x45A3;
const TAG_STRING: u32 = 0x4487;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;
const CUE_RELATIVE_POSITION: u32 = 0xF0;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const SUBTITLE_TRACK: u64 = 17;
const LACING_MASK: u8 = 0x06;

/// Positioned reads. `std::fs::File` is the production source; tests use
/// byte slices so the container layouts can be built exactly.
pub(crate) trait ReadAt: Sync {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl ReadAt for std::fs::File {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }
    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }
}

impl ReadAt for [u8] {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        let Some(available) = self.get(start..) else {
            return Ok(0);
        };
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(n)
    }
}

#[derive(Debug)]
pub(crate) enum SparseError {
    /// The cue index cannot be shown to name every block of the track.
    NotIndexed(&'static str),
    /// The file disagrees with its own index; the full demux decides.
    Malformed(&'static str),
    Io(io::Error),
}

impl fmt::Display for SparseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotIndexed(why) => write!(f, "subtitle track is not cue-indexed: {why}"),
            Self::Malformed(why) => write!(f, "matroska subtitle index is inconsistent: {why}"),
            Self::Io(error) => write!(f, "reading the matroska subtitle index: {error}"),
        }
    }
}

impl From<io::Error> for SparseError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

type Result<T> = std::result::Result<T, SparseError>;

#[derive(Clone, Copy, Debug)]
struct Header {
    id: u32,
    /// `None` is the EBML "unknown size" encoding.
    size: Option<u64>,
    len: usize,
}

fn header(bytes: &[u8], at: usize) -> Option<Header> {
    let first = *bytes.get(at)?;
    let id_len = first.leading_zeros() as usize + 1;
    if id_len > 4 {
        return None;
    }
    let id = bytes
        .get(at..at + id_len)?
        .iter()
        .fold(0u32, |id, byte| (id << 8) | u32::from(*byte));
    let size_at = at + id_len;
    let marker = *bytes.get(size_at)?;
    let size_len = marker.leading_zeros() as usize + 1;
    if size_len > 8 {
        return None;
    }
    let mut value = u64::from(marker & (0xFFu16 >> size_len) as u8);
    for byte in bytes.get(size_at + 1..size_at + size_len)? {
        value = (value << 8) | u64::from(*byte);
    }
    let unknown = value == (1u64 << (7 * size_len)) - 1;
    Some(Header {
        id,
        size: (!unknown).then_some(value),
        len: id_len + size_len,
    })
}

/// A track number inside a block: an EBML varint with its marker removed.
fn varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let marker = *bytes.first()?;
    let len = marker.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut value = u64::from(marker & (0xFFu16 >> len) as u8);
    for byte in bytes.get(1..len)? {
        value = (value << 8) | u64::from(*byte);
    }
    Some((value, len))
}

fn uint(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| {
        bytes
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
    })
}

/// `(id, whole element, body)` for every child of `body`. Metadata children
/// must have known sizes that fit their parent.
/// `(id, whole element, body)`.
type Child<'a> = (u32, &'a [u8], &'a [u8]);

fn children(body: &[u8]) -> Result<Vec<Child<'_>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let parsed = header(body, at).ok_or(SparseError::Malformed("element header"))?;
        let size = parsed
            .size
            .and_then(|size| usize::try_from(size).ok())
            .ok_or(SparseError::Malformed("unknown-size metadata element"))?;
        let end = at
            .checked_add(parsed.len)
            .and_then(|start| start.checked_add(size))
            .filter(|end| *end <= body.len())
            .ok_or(SparseError::Malformed("element overruns its parent"))?;
        out.push((parsed.id, &body[at..end], &body[at + parsed.len..end]));
        at = end;
    }
    Ok(out)
}

struct Source<'a, R: ReadAt + ?Sized> {
    reader: &'a R,
    reads: AtomicUsize,
    bytes: AtomicUsize,
}

impl<'a, R: ReadAt + ?Sized> Source<'a, R> {
    fn new(reader: &'a R) -> Self {
        Self {
            reader,
            reads: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }

    /// Up to `len` bytes; short only at end of file.
    fn read_up_to(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0; len];
        let mut filled = 0;
        while filled < len {
            let n = self
                .reader
                .read_at(&mut buf[filled..], offset + filled as u64)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(filled, Ordering::Relaxed);
        Ok(buf)
    }

    fn read_exact(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let buf = self.read_up_to(offset, len)?;
        if buf.len() == len {
            Ok(buf)
        } else {
            Err(SparseError::Malformed("element extends past end of file"))
        }
    }

    /// The whole element at `offset` (header included) when it is `id` and
    /// no larger than `max`.
    fn element(&self, offset: u64, id: u32, max: u64) -> Result<Vec<u8>> {
        let head = self.read_up_to(offset, ELEMENT_HEADER_READ)?;
        let parsed = header(&head, 0).ok_or(SparseError::Malformed("element header"))?;
        if parsed.id != id {
            return Err(SparseError::Malformed("seek entry names another element"));
        }
        let size = parsed
            .size
            .filter(|size| *size <= max)
            .ok_or(SparseError::NotIndexed(
                "metadata element is unbounded or too large",
            ))?;
        self.read_exact(offset, parsed.len + size as usize)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Reference {
    cluster: u64,
    relative: u64,
    time: u64,
}

#[derive(Debug)]
pub(crate) struct SubtitleTrack {
    /// Matroska CodecID, e.g. `S_TEXT/UTF8`.
    pub(crate) codec: String,
    number: u64,
    entry: Vec<u8>,
    references: Vec<Reference>,
    frames: Option<u64>,
    positions_complete: bool,
}

impl SubtitleTrack {
    /// Whether the cues provably name every block of this track.
    pub(crate) fn indexed(&self) -> bool {
        self.positions_complete && self.frames == Some(self.references.len() as u64)
    }

    #[cfg(test)]
    pub(crate) fn blocks(&self) -> usize {
        self.references.len()
    }
}

/// The parts of one Matroska file a subtitle subset is built from.
#[derive(Debug)]
pub(crate) struct CueIndex {
    ebml: Vec<u8>,
    info: Vec<u8>,
    segment_data: u64,
    scale: u64,
    /// Subtitle tracks in track-entry order: position is the subtitle ordinal.
    pub(crate) tracks: Vec<SubtitleTrack>,
    /// What reading the metadata cost.
    pub(crate) reads: usize,
    pub(crate) bytes_read: usize,
}

/// Read the header, seek heads, info, tracks, tags and cues. `Ok(None)` is a
/// file that is not Matroska at all.
pub(crate) fn read_index<R: ReadAt + ?Sized>(reader: &R) -> Result<Option<CueIndex>> {
    let source = Source::new(reader);
    let head = source.read_up_to(0, HEAD_BYTES)?;
    let Some(ebml) = header(&head, 0).filter(|parsed| parsed.id == EBML) else {
        return Ok(None);
    };
    let ebml_end = ebml
        .size
        .and_then(|size| usize::try_from(size).ok())
        .map(|size| ebml.len + size)
        .filter(|end| *end <= head.len())
        .ok_or(SparseError::Malformed("EBML header"))?;
    let matroska = children(&head[ebml.len..ebml_end])?
        .iter()
        .any(|(id, _, body)| *id == DOC_TYPE && matches!(*body, b"matroska" | b"webm"));
    if !matroska {
        return Ok(None);
    }
    let segment = header(&head, ebml_end)
        .filter(|parsed| parsed.id == SEGMENT)
        .ok_or(SparseError::Malformed("segment header"))?;
    let segment_data = (ebml_end + segment.len) as u64;

    let (positions, tag_positions) = locate(&source, segment_data)?;
    let position = |id: u32| {
        positions.get(&id).copied().ok_or(SparseError::NotIndexed(
            "a required top-level element is absent",
        ))
    };
    let info = source.element(segment_data + position(INFO)?, INFO, MAX_METADATA)?;
    let tracks = source.element(segment_data + position(TRACKS)?, TRACKS, MAX_METADATA)?;
    let cues = source.element(segment_data + position(CUES)?, CUES, MAX_METADATA)?;
    // mkvmerge can write global tags before the clusters and the statistics
    // tags after the cues; every one is read.
    let mut tags = Vec::new();
    for at in &tag_positions {
        tags.push(source.element(segment_data + at, TAGS, MAX_METADATA)?);
    }

    let info_header = header(&info, 0).ok_or(SparseError::Malformed("info header"))?;
    let mut scale = 1_000_000;
    for (id, _, body) in children(&info[info_header.len..])? {
        if id == TIMESTAMP_SCALE {
            scale = uint(body)
                .filter(|scale| *scale > 0)
                .ok_or(SparseError::Malformed("timestamp scale"))?;
        }
    }

    let tracks_header = header(&tracks, 0).ok_or(SparseError::Malformed("tracks header"))?;
    let mut subtitles = Vec::new();
    let mut uids = Vec::new();
    for (id, whole, body) in children(&tracks[tracks_header.len..])? {
        if id != TRACK_ENTRY {
            continue;
        }
        let (mut number, mut uid, mut kind, mut codec) = (None, None, None, None);
        for (child, _, value) in children(body)? {
            match child {
                TRACK_NUMBER => number = uint(value),
                TRACK_UID => uid = uint(value),
                TRACK_TYPE => kind = uint(value),
                CODEC_ID => codec = Some(String::from_utf8_lossy(value).into_owned()),
                _ => {}
            }
        }
        if kind != Some(SUBTITLE_TRACK) {
            continue;
        }
        subtitles.push(SubtitleTrack {
            codec: codec.ok_or(SparseError::Malformed("subtitle track without a codec"))?,
            number: number.ok_or(SparseError::Malformed("subtitle track without a number"))?,
            entry: whole.to_vec(),
            references: Vec::new(),
            frames: None,
            positions_complete: true,
        });
        uids.push(uid);
    }

    let mut frames = HashMap::new();
    for tags in &tags {
        frames.extend(frame_statistics(tags)?);
    }
    for (track, uid) in subtitles.iter_mut().zip(&uids) {
        track.frames = uid.and_then(|uid| frames.get(&uid).copied());
    }

    let cues_header = header(&cues, 0).ok_or(SparseError::Malformed("cues header"))?;
    let by_number: HashMap<u64, usize> = subtitles
        .iter()
        .enumerate()
        .map(|(ordinal, track)| (track.number, ordinal))
        .collect();
    let mut total = 0usize;
    for (id, _, point) in children(&cues[cues_header.len..])? {
        if id != CUE_POINT {
            continue;
        }
        let point = children(point)?;
        let time = point
            .iter()
            .find(|(id, _, _)| *id == CUE_TIME)
            .and_then(|(_, _, value)| uint(value))
            .ok_or(SparseError::Malformed("cue point without a time"))?;
        for (id, _, positions) in point {
            if id != CUE_TRACK_POSITIONS {
                continue;
            }
            let (mut track, mut cluster, mut relative) = (None, None, None);
            for (child, _, value) in children(positions)? {
                match child {
                    CUE_TRACK => track = uint(value),
                    CUE_CLUSTER_POSITION => cluster = uint(value),
                    CUE_RELATIVE_POSITION => relative = uint(value),
                    _ => {}
                }
            }
            let Some(&ordinal) = track.and_then(|track| by_number.get(&track)) else {
                continue;
            };
            let entry = &mut subtitles[ordinal];
            match (cluster, relative) {
                (Some(cluster), Some(relative)) => {
                    entry.references.push(Reference {
                        cluster,
                        relative,
                        time,
                    });
                    total += 1;
                    if total > MAX_REFERENCES {
                        return Err(SparseError::NotIndexed("too many subtitle cue points"));
                    }
                }
                // Without a relative position the block can only be found
                // by walking its cluster, which is the read this exists to
                // avoid. The track keeps its full demux.
                _ => entry.positions_complete = false,
            }
        }
    }
    for track in &mut subtitles {
        track.references.sort();
        track
            .references
            .dedup_by(|a, b| a.cluster == b.cluster && a.relative == b.relative);
    }

    Ok(Some(CueIndex {
        ebml: head[..ebml_end].to_vec(),
        info,
        segment_data,
        scale,
        tracks: subtitles,
        reads: source.reads.load(Ordering::Relaxed),
        bytes_read: source.bytes.load(Ordering::Relaxed),
    }))
}

/// Segment-relative positions of the top-level elements, from every seek
/// head and from the elements before the first cluster, and every `Tags`.
fn locate<R: ReadAt + ?Sized>(
    source: &Source<'_, R>,
    segment_data: u64,
) -> Result<(HashMap<u32, u64>, Vec<u64>)> {
    let mut positions = HashMap::new();
    let mut tags = Vec::new();
    let mut seek_heads = Vec::new();
    let mut at = 0u64;
    for _ in 0..MAX_TOP_LEVEL_WALK {
        let head = source.read_up_to(segment_data + at, ELEMENT_HEADER_READ)?;
        let Some(parsed) = header(&head, 0) else {
            break;
        };
        if parsed.id == CLUSTER {
            break;
        }
        positions.entry(parsed.id).or_insert(at);
        if parsed.id == TAGS {
            tags.push(at);
        }
        if parsed.id == SEEK_HEAD {
            seek_heads.push(at);
        }
        let Some(size) = parsed.size else { break };
        at += parsed.len as u64 + size;
    }
    let mut seen = Vec::new();
    while let Some(at) = seek_heads.pop() {
        if seen.contains(&at) {
            continue;
        }
        seen.push(at);
        let element = source.element(segment_data + at, SEEK_HEAD, MAX_METADATA)?;
        let parsed = header(&element, 0).ok_or(SparseError::Malformed("seek head"))?;
        for (id, _, seek) in children(&element[parsed.len..])? {
            if id != SEEK {
                continue;
            }
            let (mut target, mut position) = (None, None);
            for (child, _, value) in children(seek)? {
                match child {
                    SEEK_ID => target = uint(value).and_then(|id| u32::try_from(id).ok()),
                    SEEK_POSITION => position = uint(value),
                    _ => {}
                }
            }
            if let (Some(target), Some(position)) = (target, position) {
                positions.entry(target).or_insert(position);
                if target == TAGS {
                    tags.push(position);
                }
                if target == SEEK_HEAD {
                    seek_heads.push(position);
                }
            }
        }
    }
    tags.sort_unstable();
    tags.dedup();
    Ok((positions, tags))
}

/// `NUMBER_OF_FRAMES` per track UID, from tags that target exactly one track.
fn frame_statistics(tags: &[u8]) -> Result<HashMap<u64, u64>> {
    let parsed = header(tags, 0).ok_or(SparseError::Malformed("tags header"))?;
    let mut frames = HashMap::new();
    for (id, _, tag) in children(&tags[parsed.len..])? {
        if id != TAG {
            continue;
        }
        let tag = children(tag)?;
        let mut targets = Vec::new();
        for (id, _, body) in &tag {
            if *id == TARGETS {
                for (child, _, value) in children(body)? {
                    if child == TAG_TRACK_UID {
                        targets.extend(uint(value));
                    }
                }
            }
        }
        let &[uid] = targets.as_slice() else { continue };
        for (id, _, body) in &tag {
            if *id != SIMPLE_TAG {
                continue;
            }
            let (mut name, mut value) = (None, None);
            for (child, _, raw) in children(body)? {
                match child {
                    TAG_NAME => name = Some(raw),
                    TAG_STRING => value = std::str::from_utf8(raw).ok(),
                    _ => {}
                }
            }
            if name == Some(b"NUMBER_OF_FRAMES".as_slice()) {
                if let Some(count) = value.and_then(|value| value.trim().parse().ok()) {
                    frames.insert(uid, count);
                }
            }
        }
    }
    Ok(frames)
}

/// A subtitle-only Matroska file and what it cost to build.
#[derive(Debug)]
pub(crate) struct Subset {
    pub(crate) bytes: Vec<u8>,
    pub(crate) blocks: usize,
    pub(crate) reads: usize,
    pub(crate) bytes_read: usize,
}

/// Copy the blocks of `ordinals` into a subtitle-only Matroska file. `span`
/// keeps only blocks whose cue time lies inside it, in nanoseconds of the
/// file's own timeline. Past `deadline` the reads stop and the caller keeps
/// its demux: a fast path that has stopped being fast is not one.
pub(crate) fn subset<R: ReadAt + ?Sized>(
    reader: &R,
    index: &CueIndex,
    ordinals: &[usize],
    span: Option<(i128, i128)>,
    deadline: std::time::Instant,
) -> Result<Subset> {
    let mut wanted = Vec::new();
    for &ordinal in ordinals {
        let track = index
            .tracks
            .get(ordinal)
            .ok_or(SparseError::NotIndexed("no such subtitle ordinal"))?;
        if !track.indexed() {
            return Err(SparseError::NotIndexed(
                "cue references do not account for every frame",
            ));
        }
        for reference in &track.references {
            let time = i128::from(reference.time) * i128::from(index.scale);
            if span.is_none_or(|(from, to)| time >= from && time <= to) {
                wanted.push((*reference, track.number));
            }
        }
    }
    // File order, which is the order the full demux hands packets over.
    wanted.sort();
    wanted.dedup_by(|a, b| a.0.cluster == b.0.cluster && a.0.relative == b.0.relative);

    let source = Source::new(reader);
    let mut clusters: Vec<u64> = wanted
        .iter()
        .map(|(reference, _)| reference.cluster)
        .collect();
    clusters.dedup();
    let headers = parallel(&clusters, |cluster| {
        let at = index.segment_data + cluster;
        let head = source.read_up_to(at, CLUSTER_HEADER_READ)?;
        let parsed = header(&head, 0)
            .filter(|parsed| parsed.id == CLUSTER)
            .ok_or(SparseError::Malformed("cue does not point at a cluster"))?;
        let mut child = parsed.len;
        while let Some(found) = header(&head, child) {
            let size = found
                .size
                .and_then(|size| usize::try_from(size).ok())
                .ok_or(SparseError::Malformed("cluster child size"))?;
            if found.id == CLUSTER_TIMESTAMP {
                let value = head
                    .get(child + found.len..child + found.len + size)
                    .and_then(uint)
                    .ok_or(SparseError::Malformed("cluster timestamp"))?;
                return Ok((value, at + parsed.len as u64, parsed.size));
            }
            child += found.len + size;
        }
        Err(SparseError::Malformed(
            "cluster timestamp is not in its header",
        ))
    })?;
    let headers: HashMap<u64, (u64, u64, Option<u64>)> =
        clusters.into_iter().zip(headers).collect();

    let copied = AtomicUsize::new(0);
    let blocks = parallel(&wanted, |(reference, number)| {
        if std::time::Instant::now() > deadline {
            return Err(SparseError::NotIndexed("cue walk outran its deadline"));
        }
        let (timestamp, data, cluster_size) = headers[&reference.cluster];
        let at = data + reference.relative;
        let mut bytes = source.read_up_to(at, BLOCK_READ)?;
        let parsed = header(&bytes, 0).ok_or(SparseError::Malformed("block header"))?;
        if parsed.id != SIMPLE_BLOCK && parsed.id != BLOCK_GROUP {
            return Err(SparseError::Malformed("cue does not point at a block"));
        }
        let size = parsed
            .size
            .filter(|size| *size <= MAX_BLOCK)
            .ok_or(SparseError::Malformed("block size"))?;
        let total = parsed.len + size as usize;
        if total > bytes.len() {
            bytes = source.read_exact(at, total)?;
        }
        if cluster_size.is_some_and(|size| reference.relative + total as u64 > size) {
            return Err(SparseError::Malformed("cue points past its cluster"));
        }
        if copied.fetch_add(total, Ordering::Relaxed) + total > MAX_SUBSET_BYTES {
            return Err(SparseError::NotIndexed("subtitle subset is too large"));
        }
        bytes.truncate(total);
        bytes.shrink_to_fit();
        let block = if parsed.id == SIMPLE_BLOCK {
            &bytes[parsed.len..]
        } else {
            children(&bytes[parsed.len..])?
                .into_iter()
                .find(|(id, _, _)| *id == BLOCK)
                .map(|(_, _, body)| body)
                .ok_or(SparseError::Malformed("block group without a block"))?
        };
        let (track, len) = varint(block).ok_or(SparseError::Malformed("block track"))?;
        if track != *number {
            return Err(SparseError::Malformed(
                "cue points at another track's block",
            ));
        }
        let relative = block
            .get(len..len + 2)
            .map(|raw| i16::from_be_bytes([raw[0], raw[1]]))
            .ok_or(SparseError::Malformed("block timestamp"))?;
        // The span was chosen on cue times; the subset carries block times.
        // They are the same instant in a well-formed file, and a file where
        // they are not cannot be trusted to have been windowed correctly.
        if i128::from(timestamp) + i128::from(relative) != i128::from(reference.time) {
            return Err(SparseError::Malformed("cue time is not its block's time"));
        }
        let flags = *block
            .get(len + 2)
            .ok_or(SparseError::Malformed("block flags"))?;
        if flags & LACING_MASK != 0 {
            // A laced block holds several frames, so a frame count no
            // longer equals a block count and the proof above is void.
            return Err(SparseError::NotIndexed("laced subtitle block"));
        }
        Ok(bytes)
    })?;

    let mut entries = Vec::new();
    for track in &index.tracks {
        entries.extend_from_slice(&track.entry);
    }
    let mut body = index.info.clone();
    body.extend(element(TRACKS, &entries));
    let mut at = 0;
    while at < wanted.len() {
        let cluster = wanted[at].0.cluster;
        let mut content = element(CLUSTER_TIMESTAMP, &headers[&cluster].0.to_be_bytes());
        while at < wanted.len() && wanted[at].0.cluster == cluster {
            content.extend_from_slice(&blocks[at]);
            at += 1;
        }
        body.extend(element(CLUSTER, &content));
    }
    let mut bytes = index.ebml.clone();
    bytes.extend(element(SEGMENT, &body));
    Ok(Subset {
        bytes,
        blocks: wanted.len(),
        reads: source.reads.load(Ordering::Relaxed),
        bytes_read: source.bytes.load(Ordering::Relaxed),
    })
}

fn element(id: u32, body: &[u8]) -> Vec<u8> {
    let id_bytes = id.to_be_bytes();
    let skip = id_bytes.iter().take_while(|byte| **byte == 0).count();
    let mut out = id_bytes[skip..].to_vec();
    out.extend((0x0100_0000_0000_0000u64 | body.len() as u64).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Map `items` over `READERS` threads, keeping order. The first error wins.
fn parallel<T: Sync, U: Send>(items: &[T], f: impl Fn(&T) -> Result<U> + Sync) -> Result<Vec<U>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let chunk = items.len().div_ceil(READERS);
    std::thread::scope(|scope| {
        let workers: Vec<_> = items
            .chunks(chunk)
            .map(|part| {
                let f = &f;
                scope.spawn(move || part.iter().map(f).collect::<Result<Vec<U>>>())
            })
            .collect();
        let mut out = Vec::with_capacity(items.len());
        for worker in workers {
            out.extend(
                worker
                    .join()
                    .map_err(|_| SparseError::Malformed("subtitle reader panicked"))??,
            );
        }
        Ok(out)
    })
}

/// FFmpeg's name for a Matroska subtitle codec, as the library scan stores it.
pub(crate) fn same_codec(matroska: &str, probed: &str) -> bool {
    match matroska {
        "S_TEXT/UTF8" => matches!(probed, "subrip" | "srt"),
        "S_TEXT/ASCII" => probed == "text",
        "S_TEXT/ASS" | "S_TEXT/SSA" | "S_ASS" | "S_SSA" => matches!(probed, "ass" | "ssa"),
        "S_TEXT/WEBVTT" => probed == "webvtt",
        "S_HDMV/PGS" => probed == "hdmv_pgs_subtitle",
        "S_VOBSUB" => probed == "dvd_subtitle",
        "S_DVBSUB" => probed == "dvb_subtitle",
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Matroska files laid out byte for byte, so each test controls exactly
    //! which index facts are present.
    use super::*;

    pub(crate) fn uint_element(id: u32, value: u64) -> Vec<u8> {
        element(id, &value.to_be_bytes())
    }

    pub(crate) struct Block {
        pub track: u64,
        pub relative: i16,
        pub payload: Vec<u8>,
        pub duration: Option<u64>,
        pub flags: u8,
    }

    pub(crate) struct Fixture {
        pub clusters: Vec<(u64, Vec<Block>)>,
        /// Track number, CodecID.
        pub tracks: Vec<(u64, u64, &'static str)>,
        /// Track number → NUMBER_OF_FRAMES statistic to write.
        pub frames: Vec<(u64, u64)>,
        /// Leave these (cluster, block) positions out of the cues.
        pub uncued: Vec<(usize, usize)>,
        /// Write these cues without CueRelativePosition.
        pub no_relative: bool,
        /// Point the first subtitle cue at this (cluster, block) instead.
        pub misdirect: Option<(usize, usize)>,
        /// Added to every subtitle cue's time.
        pub cue_time_skew: u64,
        /// Write the first statistic in a `Tags` before the clusters and
        /// the rest in a second one after the cues.
        pub split_tags: bool,
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                clusters: vec![
                    (
                        0,
                        vec![
                            Block::media(0),
                            Block::text(2, 100, "first line", 900),
                            Block::media(40),
                        ],
                    ),
                    (
                        5000,
                        vec![
                            Block::media(0),
                            Block::text(2, 250, "second line", 1200),
                            Block::text(3, 300, "bitmap", 500),
                        ],
                    ),
                    (
                        10_000,
                        vec![Block::media(0), Block::text(2, 10, "third line", 800)],
                    ),
                ],
                tracks: vec![
                    (1, 2, "A_PCM/INT/LIT"),
                    (2, 17, "S_TEXT/UTF8"),
                    (3, 17, "S_HDMV/PGS"),
                ],
                frames: vec![(2, 3), (3, 1)],
                uncued: Vec::new(),
                no_relative: false,
                misdirect: None,
                cue_time_skew: 0,
                split_tags: false,
            }
        }
    }

    impl Block {
        /// A block of the PCM track: 20 KB, recognisable, and valid audio
        /// so FFprobe can measure the file's start from it.
        pub(crate) fn media(relative: i16) -> Self {
            Self {
                track: 1,
                relative,
                payload: vec![0x5A; 20_000],
                duration: None,
                flags: 0x80,
            }
        }
        pub(crate) fn text(track: u64, relative: i16, text: &str, duration: u64) -> Self {
            Self {
                track,
                relative,
                payload: text.as_bytes().to_vec(),
                duration: Some(duration),
                flags: 0,
            }
        }
        fn bytes(&self) -> Vec<u8> {
            let mut block = vec![0x80 | self.track as u8];
            block.extend(self.relative.to_be_bytes());
            block.push(self.flags);
            block.extend(&self.payload);
            match self.duration {
                None => element(SIMPLE_BLOCK, &block),
                Some(duration) => {
                    let mut group = element(BLOCK, &block);
                    group.extend(uint_element(0x9B, duration));
                    element(BLOCK_GROUP, &group)
                }
            }
        }
    }

    impl Fixture {
        pub(crate) fn build(&self) -> Vec<u8> {
            let mut ebml_body = uint_element(0x4286, 1);
            ebml_body.extend(element(DOC_TYPE, b"matroska"));
            let ebml = element(EBML, &ebml_body);

            let mut info = uint_element(TIMESTAMP_SCALE, 1_000_000);
            info.extend(element(0x4D80, b"fixture"));
            let info = element(INFO, &info);
            let mut entries = Vec::new();
            for (number, kind, codec) in &self.tracks {
                let mut entry = uint_element(TRACK_NUMBER, *number);
                entry.extend(uint_element(TRACK_UID, 1000 + number));
                entry.extend(uint_element(TRACK_TYPE, *kind));
                entry.extend(element(CODEC_ID, codec.as_bytes()));
                if *kind == 2 {
                    // A 5 ms codec delay, as Opus and AAC tracks carry,
                    // gives the file a start time other than zero, so
                    // FFmpeg's default timeline moves every cue.
                    entry.extend(uint_element(0x56AA, 5_000_000));
                    let mut audio = element(0xB5, &48_000f64.to_be_bytes());
                    audio.extend(uint_element(0x9F, 2));
                    audio.extend(uint_element(0x6264, 16));
                    entry.extend(element(0xE1, &audio));
                }
                entries.extend(element(TRACK_ENTRY, &entry));
            }
            let tracks = element(TRACKS, &entries);
            let mut tag_lists = (Vec::new(), Vec::new());
            for (n, (number, frames)) in self.frames.iter().enumerate() {
                let targets = element(TARGETS, &uint_element(TAG_TRACK_UID, 1000 + number));
                let mut simple = element(TAG_NAME, b"NUMBER_OF_FRAMES");
                simple.extend(element(TAG_STRING, frames.to_string().as_bytes()));
                let mut tag = targets;
                tag.extend(element(SIMPLE_TAG, &simple));
                if self.split_tags && n > 0 {
                    tag_lists.1.extend(element(TAG, &tag));
                } else {
                    tag_lists.0.extend(element(TAG, &tag));
                }
            }
            let tags = element(TAGS, &tag_lists.0);
            let late_tags = (!tag_lists.1.is_empty()).then(|| element(TAGS, &tag_lists.1));

            // Fixed-size seek head first so positions are known up front.
            let seek_len = element(
                SEEK_HEAD,
                &seek_entries(&[(INFO, 0), (TRACKS, 0), (TAGS, 0), (CUES, 0)]),
            )
            .len() as u64;
            let info_at = seek_len;
            let tracks_at = info_at + info.len() as u64;
            let tags_at = tracks_at + tracks.len() as u64;
            let mut clusters = Vec::new();
            let mut positions = Vec::new();
            let mut at = tags_at + tags.len() as u64;
            for (timestamp, blocks) in &self.clusters {
                let mut content = uint_element(CLUSTER_TIMESTAMP, *timestamp);
                let mut relatives = Vec::new();
                for block in blocks {
                    relatives.push(content.len() as u64);
                    content.extend(block.bytes());
                }
                positions.push((at, relatives));
                let cluster = element(CLUSTER, &content);
                at += cluster.len() as u64;
                clusters.extend(cluster);
            }
            let cues_at = at;
            let mut points = Vec::new();
            let mut first_subtitle = true;
            for (c, (timestamp, blocks)) in self.clusters.iter().enumerate() {
                for (b, block) in blocks.iter().enumerate() {
                    if self.uncued.contains(&(c, b)) {
                        continue;
                    }
                    let subtitle = self
                        .tracks
                        .iter()
                        .any(|(n, k, _)| *n == block.track && *k == 17);
                    if !subtitle {
                        continue;
                    }
                    let (mut pc, mut pb) = (c, b);
                    if subtitle && first_subtitle {
                        first_subtitle = false;
                        if let Some(target) = self.misdirect {
                            (pc, pb) = target;
                        }
                    }
                    let time =
                        (*timestamp as i64 + i64::from(block.relative)) as u64 + self.cue_time_skew;
                    let mut positions_body = uint_element(CUE_TRACK, block.track);
                    positions_body.extend(uint_element(CUE_CLUSTER_POSITION, positions[pc].0));
                    if !(subtitle && self.no_relative) {
                        positions_body
                            .extend(uint_element(CUE_RELATIVE_POSITION, positions[pc].1[pb]));
                    }
                    let mut point = uint_element(CUE_TIME, time);
                    point.extend(element(CUE_TRACK_POSITIONS, &positions_body));
                    points.extend(element(CUE_POINT, &point));
                }
            }
            let cues = element(CUES, &points);
            // Split tags: the early one is found by walking the top level,
            // the seek head names only the late one.
            let named_tags = if late_tags.is_some() {
                cues_at + cues.len() as u64
            } else {
                tags_at
            };
            let seek = element(
                SEEK_HEAD,
                &seek_entries(&[
                    (INFO, info_at),
                    (TRACKS, tracks_at),
                    (TAGS, named_tags),
                    (CUES, cues_at),
                ]),
            );
            assert_eq!(seek.len() as u64, seek_len);
            let mut segment = seek;
            segment.extend(info);
            segment.extend(tracks);
            segment.extend(tags);
            segment.extend(clusters);
            segment.extend(cues);
            segment.extend(late_tags.unwrap_or_default());
            let mut file = ebml;
            file.extend(element(SEGMENT, &segment));
            file
        }
    }

    fn seek_entries(entries: &[(u32, u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (id, position) in entries {
            let mut seek = element(SEEK_ID, &id.to_be_bytes());
            seek.extend(uint_element(SEEK_POSITION, *position));
            out.extend(element(SEEK, &seek));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::Fixture;
    use super::*;

    fn later() -> std::time::Instant {
        std::time::Instant::now() + std::time::Duration::from_secs(60)
    }

    fn index(file: &[u8]) -> CueIndex {
        read_index(file).expect("index").expect("matroska")
    }

    /// Every block of `track` in a parsed subset, as (absolute ms, payload).
    fn blocks_of(subset: &[u8], track: u64) -> Vec<(i64, Vec<u8>)> {
        let ebml = header(subset, 0).expect("fixture");
        let at = ebml.len + ebml.size.expect("fixture") as usize;
        let segment = header(subset, at).expect("fixture");
        let mut out = Vec::new();
        for (id, _, cluster) in children(&subset[at + segment.len..]).expect("fixture") {
            if id != CLUSTER {
                continue;
            }
            let content = children(cluster).expect("fixture");
            let timestamp = uint(content[0].2).expect("fixture") as i64;
            for (id, _, body) in &content[1..] {
                let block = if *id == SIMPLE_BLOCK {
                    *body
                } else {
                    children(body).expect("fixture")[0].2
                };
                let (number, len) = varint(block).expect("fixture");
                if number == track {
                    let relative = i16::from_be_bytes([block[len], block[len + 1]]);
                    out.push((timestamp + i64::from(relative), block[len + 3..].to_vec()));
                }
            }
        }
        out
    }

    #[test]
    fn subtitle_tracks_are_listed_in_ordinal_order_with_their_proof() {
        let index = index(&Fixture::default().build());
        let codecs: Vec<_> = index
            .tracks
            .iter()
            .map(|track| track.codec.as_str())
            .collect();
        assert_eq!(codecs, ["S_TEXT/UTF8", "S_HDMV/PGS"]);
        assert!(index.tracks[0].indexed());
        assert!(index.tracks[1].indexed());
        assert_eq!(index.tracks[0].blocks(), 3);
    }

    #[test]
    fn the_subset_holds_every_cued_subtitle_block_and_no_picture() {
        let file = Fixture::default().build();
        let index = index(&file);
        let subset = subset(file.as_slice(), &index, &[0], None, later()).expect("fixture");
        assert_eq!(
            blocks_of(&subset.bytes, 2),
            vec![
                (100, b"first line".to_vec()),
                (5250, b"second line".to_vec()),
                (10_010, b"third line".to_vec()),
            ]
        );
        assert!(
            blocks_of(&subset.bytes, 3).is_empty(),
            "only the asked-for track"
        );
        assert!(blocks_of(&subset.bytes, 1).is_empty());
        assert!(!subset
            .bytes
            .windows(64)
            .any(|w| w.iter().all(|b| *b == 0x5A)));
        // Cluster headers and subtitle blocks only: less than one picture
        // block's payload.
        assert!(subset.bytes_read < 20_000, "{}", subset.bytes_read);
    }

    #[test]
    fn the_subset_keeps_every_subtitle_entry_so_ordinals_do_not_move() {
        let file = Fixture::default().build();
        let index = index(&file);
        let subset = subset(file.as_slice(), &index, &[1], None, later()).expect("fixture");
        assert_eq!(
            blocks_of(&subset.bytes, 3),
            vec![(5300, b"bitmap".to_vec())]
        );
        for track in &index.tracks {
            assert!(subset
                .bytes
                .windows(track.entry.len())
                .any(|w| w == track.entry));
        }
    }

    #[test]
    fn a_span_keeps_only_blocks_cued_inside_it() {
        let file = Fixture::default().build();
        let index = index(&file);
        let subset = subset(
            file.as_slice(),
            &index,
            &[0],
            Some((5_000_000_000, 9_000_000_000)),
            later(),
        )
        .expect("fixture");
        assert_eq!(
            blocks_of(&subset.bytes, 2),
            vec![(5250, b"second line".to_vec())]
        );
        assert_eq!(subset.blocks, 1);
    }

    #[test]
    fn a_frame_statistic_the_cues_do_not_match_keeps_the_full_demux() {
        let fixture = Fixture {
            uncued: vec![(2, 1)],
            ..Fixture::default()
        };
        let file = fixture.build();
        let index = index(&file);
        assert!(!index.tracks[0].indexed(), "two cues for three frames");
        assert!(matches!(
            subset(file.as_slice(), &index, &[0], None, later()),
            Err(SparseError::NotIndexed(_))
        ));
    }

    #[test]
    fn no_statistic_means_no_proof() {
        let fixture = Fixture {
            frames: Vec::new(),
            ..Fixture::default()
        };
        let index = index(&fixture.build());
        assert!(!index.tracks[0].indexed());
    }

    #[test]
    fn cues_without_relative_positions_are_not_an_index() {
        let fixture = Fixture {
            no_relative: true,
            ..Fixture::default()
        };
        let index = index(&fixture.build());
        assert!(!index.tracks[0].indexed());
    }

    #[test]
    fn a_cue_that_lands_on_another_tracks_block_is_refused() {
        let fixture = Fixture {
            misdirect: Some((0, 0)),
            ..Fixture::default()
        };
        let file = fixture.build();
        let index = index(&file);
        assert!(matches!(
            subset(file.as_slice(), &index, &[0], None, later()),
            Err(SparseError::Malformed(_))
        ));
    }

    #[test]
    fn a_cue_time_that_is_not_its_blocks_time_is_refused() {
        // The window is chosen on cue times; a block that sits elsewhere in
        // time would be windowed wrongly.
        let fixture = Fixture {
            cue_time_skew: 7,
            ..Fixture::default()
        };
        let file = fixture.build();
        let index = index(&file);
        assert!(matches!(
            subset(file.as_slice(), &index, &[0], None, later()),
            Err(SparseError::Malformed(_))
        ));
    }

    #[test]
    fn a_walk_past_its_deadline_keeps_the_full_demux() {
        let file = Fixture::default().build();
        let index = index(&file);
        assert!(matches!(
            subset(
                file.as_slice(),
                &index,
                &[0],
                None,
                std::time::Instant::now()
            ),
            Err(SparseError::NotIndexed(_))
        ));
    }

    #[test]
    fn statistics_in_a_second_tags_element_still_prove_the_track() {
        let fixture = Fixture {
            split_tags: true,
            ..Fixture::default()
        };
        let index = index(&fixture.build());
        assert!(index.tracks[0].indexed());
        assert!(index.tracks[1].indexed());
    }

    #[test]
    fn laced_subtitle_blocks_void_the_frame_count() {
        let mut fixture = Fixture::default();
        fixture.clusters[1].1[1].flags = 0x02;
        let file = fixture.build();
        let index = index(&file);
        assert!(matches!(
            subset(file.as_slice(), &index, &[0], None, later()),
            Err(SparseError::NotIndexed(_))
        ));
    }

    #[test]
    fn a_file_that_is_not_matroska_is_not_an_error() {
        assert!(read_index(b"\x00\x00\x00\x18ftypmp42".as_slice())
            .expect("fixture")
            .is_none());
    }

    #[test]
    fn unknown_sizes_and_long_varints_parse() {
        let parsed = header(
            &[
                0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            ],
            0,
        )
        .expect("fixture");
        assert_eq!((parsed.id, parsed.size, parsed.len), (SEGMENT, None, 12));
        let parsed = header(&[0xE7, 0x81, 0x05], 0).expect("fixture");
        assert_eq!(
            (parsed.id, parsed.size, parsed.len),
            (CLUSTER_TIMESTAMP, Some(1), 2)
        );
        assert_eq!(varint(&[0x40, 0x02]), Some((2, 2)));
    }

    #[test]
    fn scan_codecs_map_onto_matroska_codec_ids() {
        assert!(same_codec("S_TEXT/UTF8", "subrip"));
        assert!(same_codec("S_TEXT/ASS", "ass"));
        assert!(same_codec("S_HDMV/PGS", "hdmv_pgs_subtitle"));
        assert!(!same_codec("S_TEXT/UTF8", "ass"));
        assert!(!same_codec("S_HDMV/TEXTST", "hdmv_text_subtitle"));
    }
}
