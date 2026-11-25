//! A video-only fragment kept without its coded slice data.
//!
//! A verified copy's canonical output exists to be measured, never served:
//! its index row, its cut class, its parameter sets, its HDR10 SEI, its Dolby
//! Vision RPUs and its packet clock are read, and then it is dropped. None of
//! those reads the inside of a slice, so [`FragmentTrimmer`] drops slice data
//! as the fragment arrives and keeps a count of what it dropped.
//!
//! What is kept is itself a well-formed fragment ([`TrimmedFragment::kept`]):
//! the `moof` unchanged in shape, declaring what is kept; every non-VCL NAL
//! unit whole, wherever it sits in its sample; and each VCL NAL unit as its
//! length prefix and two-byte header. So every reader of a fragment reads the
//! kept one unchanged, the real conversion included, and each wire quantity
//! is the kept one plus what was dropped.

use super::{
    be_u32, children, parse_moof, peek_box, vcl_nal_type, Fmp4Error, Fragment, Init, Track,
    VideoCodec, MAX_BOX_BYTES,
};

/// How much of a VCL NAL unit is kept: its two-byte header (for H.264, its
/// one-byte header and the byte after).
const KEPT_VCL_BYTES: usize = 2;

/// A video-only fragment and what was dropped from each of its samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimmedFragment {
    kept: Fragment,
    /// Slice-data bytes dropped from each video sample, in decode order.
    /// Empty when nothing was dropped.
    dropped: Vec<u32>,
}

impl TrimmedFragment {
    /// A fragment held whole: nothing dropped.
    pub fn whole(fragment: Fragment) -> Self {
        Self {
            kept: fragment,
            dropped: Vec::new(),
        }
    }

    /// What is held: a well-formed fragment of the kept bytes.
    pub fn kept(&self) -> &Fragment {
        &self.kept
    }

    /// The kept fragment, for a sample rewrite. A rewrite keeps the sample
    /// count, which is what ties each sample to what was dropped from it.
    pub fn kept_mut(&mut self) -> &mut Fragment {
        &mut self.kept
    }

    /// Bytes held in memory.
    pub fn held_len(&self) -> usize {
        self.kept.len()
    }

    /// Bytes the fragment has on the wire, as [`Fragment::len`] of the whole
    /// one counts them.
    pub fn wire_len(&self) -> u64 {
        VideoOnlyFragment::from(self).wire_len()
    }
}

/// A canonical video-only fragment as its measurers read it: whole, or kept
/// with a count of what was dropped from each sample.
#[derive(Debug, Clone, Copy)]
pub struct VideoOnlyFragment<'a> {
    fragment: &'a Fragment,
    dropped: &'a [u32],
}

impl<'a> From<&'a Fragment> for VideoOnlyFragment<'a> {
    fn from(fragment: &'a Fragment) -> Self {
        Self {
            fragment,
            dropped: &[],
        }
    }
}

impl<'a> From<&'a TrimmedFragment> for VideoOnlyFragment<'a> {
    fn from(trimmed: &'a TrimmedFragment) -> Self {
        Self {
            fragment: &trimmed.kept,
            dropped: &trimmed.dropped,
        }
    }
}

impl<'a> VideoOnlyFragment<'a> {
    /// The fragment as held. Its sample sizes and length omit what was
    /// dropped; everything else is the whole fragment's.
    pub fn fragment(&self) -> &'a Fragment {
        self.fragment
    }

    /// Bytes dropped from the video sample at `index` in decode order.
    pub fn dropped_from(&self, index: usize) -> u32 {
        self.dropped.get(index).copied().unwrap_or(0)
    }

    /// The wire size of the video sample at `index`, of size `kept` as held.
    pub fn sample_size(&self, index: usize, kept: u32) -> u64 {
        u64::from(kept) + u64::from(self.dropped_from(index))
    }

    fn dropped_total(&self) -> u64 {
        self.dropped.iter().map(|bytes| u64::from(*bytes)).sum()
    }

    /// Bytes the whole fragment has on the wire.
    pub fn wire_len(&self) -> u64 {
        self.fragment.len() as u64 + self.dropped_total()
    }
}

/// One video sample's place in the arriving `mdat`.
struct Span {
    start: usize,
    end: usize,
    /// Where this sample's size is declared, when a new size can be written
    /// there. A sample without one is kept whole.
    size_field: Option<usize>,
    size: u32,
}

/// Drops slice data from one video-only fragment as its `mdat` arrives.
///
/// Whatever it cannot place exactly is kept whole: a fragment that is not the
/// init's single video track, samples outside the `mdat` or out of order, a
/// sample whose size has nowhere to be rewritten, and the rest of a sample
/// from the first NAL unit that does not fit it. A reader therefore meets the
/// same malformed bytes it would have met in the whole fragment.
pub struct FragmentTrimmer {
    /// The `moof`, the `mdat` header and the kept payload so far.
    out: Vec<u8>,
    tracks: Vec<Track>,
    moof_len: usize,
    mdat_header: usize,
    /// Offset in the arriving fragment of the next byte to be pushed.
    at: usize,
    /// The arriving fragment's length.
    end: usize,
    spans: Vec<Span>,
    next: usize,
    dropped: Vec<u32>,
    codec: VideoCodec,
    length_size: usize,
    /// The NAL length prefix and first byte being collected.
    head: [u8; 5],
    head_len: usize,
    /// Bytes of the current NAL unit still to keep, then to drop.
    keep: usize,
    drop: usize,
    /// The rest of the current sample is kept as it is.
    verbatim: bool,
}

impl FragmentTrimmer {
    /// Begin a fragment from its complete `moof` and its `mdat` box header.
    pub fn begin(moof: Vec<u8>, mdat_header: &[u8], init: &Init) -> Result<Self, Fmp4Error> {
        let moof_header = peek_box(&moof, 0)?
            .filter(|header| header.kind() == b"moof" && header.size == moof.len())
            .ok_or_else(|| Fmp4Error::Malformed("a fragment does not begin with a moof".into()))?;
        let mdat = peek_box(mdat_header, 0)?
            .filter(|header| header.kind() == b"mdat" && header.header_len == mdat_header.len())
            .ok_or_else(|| Fmp4Error::Malformed("expected mdat after moof".into()))?;
        let moof_len = moof.len();
        let payload_start = moof_len + mdat.header_len;
        let end = moof_len
            .checked_add(mdat.size)
            .filter(|end| *end <= MAX_BOX_BYTES)
            .ok_or_else(|| Fmp4Error::Unsupported("a fragment is too large".into()))?;
        let parsed = parse_moof(&moof, moof_header.header_len, &init.tracks)?;

        let mut spans = Vec::new();
        let mut codec = VideoCodec::Hevc;
        let mut length_size = 0usize;
        if let (Some(video), [track], 1) = (init.video(), parsed.as_slice(), init.tracks.len()) {
            if let (Some(known), true, 1 | 2 | 4) = (
                video.codec,
                track.track_id == video.id,
                video.nal_length_size,
            ) {
                codec = known;
                length_size = usize::from(video.nal_length_size);
                // A size taken from the `tfhd` can be rewritten there only
                // when a single sample takes it.
                let defaulted = track
                    .samples()
                    .filter(|sample| sample.size_at.is_none())
                    .count();
                let default_field = (defaulted == 1)
                    .then(|| default_sample_size_field(&moof, moof_header.header_len))
                    .flatten();
                let mut previous_end = payload_start;
                let mut placed = true;
                'runs: for run in &track.runs {
                    let mut at = run.data_offset;
                    for sample in &run.samples {
                        let Some(sample_end) = at
                            .checked_add(sample.size as usize)
                            .filter(|sample_end| at >= previous_end && *sample_end <= end)
                        else {
                            placed = false;
                            break 'runs;
                        };
                        let size_field = sample
                            .size_at
                            .or(default_field.filter(|field| be_u32(&moof, *field) == sample.size));
                        spans.push(Span {
                            start: at,
                            end: sample_end,
                            size_field,
                            size: sample.size,
                        });
                        at = sample_end;
                        previous_end = sample_end;
                    }
                }
                if !placed {
                    spans.clear();
                }
            }
        }

        let dropped = vec![0; spans.len()];
        let mut out = moof;
        out.extend_from_slice(mdat_header);
        Ok(Self {
            out,
            tracks: init.tracks.clone(),
            moof_len,
            mdat_header: mdat.header_len,
            at: payload_start,
            end,
            spans,
            next: 0,
            dropped,
            codec,
            length_size,
            head: [0; 5],
            head_len: 0,
            keep: 0,
            drop: 0,
            verbatim: false,
        })
    }

    /// Bytes held now.
    pub fn held(&self) -> usize {
        self.out.len() + self.head_len
    }

    /// Feed the next bytes of the `mdat` payload.
    pub fn push(&mut self, mut input: &[u8]) {
        while !input.is_empty() {
            while self.spans.get(self.next).is_some_and(|s| self.at >= s.end) {
                self.next += 1;
                self.verbatim = false;
            }
            let span = self
                .spans
                .get(self.next)
                .map(|span| (span.start, span.end, span.size_field.is_some()));
            let Some((_, end, trimmed)) = span.filter(|(start, ..)| self.at >= *start) else {
                // Between samples: kept as it is.
                let until = span.map_or(usize::MAX, |(start, ..)| start);
                input = self.keep_bytes(input, until - self.at);
                continue;
            };
            let remaining = end - self.at;
            if !trimmed || self.verbatim {
                input = self.keep_bytes(input, remaining);
            } else if self.keep > 0 {
                let kept = self.keep.min(input.len());
                self.keep -= kept;
                input = self.keep_bytes(input, kept);
            } else if self.drop > 0 {
                let dropped = self.drop.min(input.len());
                self.drop -= dropped;
                self.at += dropped;
                input = &input[dropped..];
            } else if self.head_len == 0 && remaining < self.length_size {
                // A truncated length prefix.
                self.verbatim = true;
            } else {
                self.head[self.head_len] = input[0];
                self.head_len += 1;
                self.at += 1;
                input = &input[1..];
                self.framed(end - self.at);
            }
        }
    }

    fn keep_bytes<'a>(&mut self, input: &'a [u8], at_most: usize) -> &'a [u8] {
        let (kept, rest) = input.split_at(at_most.min(input.len()));
        self.out.extend_from_slice(kept);
        self.at += kept.len();
        rest
    }

    /// Decide the NAL unit whose length prefix, and then first byte, has
    /// arrived, with `remaining` bytes of its sample after what was read.
    fn framed(&mut self, remaining: usize) {
        if self.head_len < self.length_size {
            return;
        }
        let prefix = &self.head[..self.length_size];
        let length = prefix
            .iter()
            .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
        if self.head_len == self.length_size {
            if length == 0 || length > remaining {
                // A unit that does not fit its sample: no reader gets past
                // it, and each must fail on it as it would have.
                self.out.extend_from_slice(prefix);
                self.head_len = 0;
                self.verbatim = true;
            }
            return;
        }
        let first = self.head[self.length_size];
        let (_, vcl) = vcl_nal_type(first, self.codec);
        if vcl && length > KEPT_VCL_BYTES {
            let kept = (KEPT_VCL_BYTES as u32).to_be_bytes();
            self.out.extend_from_slice(&kept[4 - self.length_size..]);
            self.out.push(first);
            self.keep = KEPT_VCL_BYTES - 1;
            self.drop = length - KEPT_VCL_BYTES;
            // Bounded by the sample's own 32-bit size.
            self.dropped[self.next] += self.drop as u32;
        } else {
            self.out.extend_from_slice(&self.head[..self.head_len]);
            self.keep = length - 1;
        }
        self.head_len = 0;
    }

    /// The fragment, once its whole `mdat` has been pushed.
    pub fn finish(mut self) -> Result<TrimmedFragment, Fmp4Error> {
        if self.at != self.end || self.head_len != 0 || self.keep != 0 || self.drop != 0 {
            return Err(Fmp4Error::Malformed(
                "a fragment ended inside its mdat".into(),
            ));
        }
        let header_len = peek_box(&self.out, 0)?
            .ok_or_else(|| Fmp4Error::Malformed("a fragment with no moof".into()))?
            .header_len;
        let payload_start = self.moof_len + self.mdat_header;
        if self.dropped.iter().all(|dropped| *dropped == 0) {
            let tracks = parse_moof(&self.out[..self.moof_len], header_len, &self.tracks)?;
            return Ok(TrimmedFragment {
                kept: Fragment {
                    mdat_payload: payload_start..self.out.len(),
                    bytes: self.out,
                    tracks,
                },
                dropped: Vec::new(),
            });
        }

        // Declare what is kept: each sample's size, each run's start, and the
        // `mdat`. Every field keeps its width, so the `moof` keeps its shape.
        let original = parse_moof(&self.out[..self.moof_len], header_len, &self.tracks)?;
        for (span, dropped) in self.spans.iter().zip(&self.dropped) {
            if let (Some(field), true) = (span.size_field, *dropped != 0) {
                self.out[field..field + 4].copy_from_slice(&(span.size - dropped).to_be_bytes());
            }
        }
        for run in original.iter().flat_map(|track| &track.runs) {
            let Some(field) = run.data_offset_at else {
                continue;
            };
            let before: usize = self
                .spans
                .iter()
                .zip(&self.dropped)
                .filter(|(span, _)| span.start < run.data_offset)
                .map(|(_, dropped)| *dropped as usize)
                .sum();
            let moved = u32::try_from(run.data_offset - before)
                .map_err(|_| Fmp4Error::Unsupported("a trun data_offset does not fit".into()))?;
            self.out[field..field + 4].copy_from_slice(&moved.to_be_bytes());
        }
        let mdat_len = self.out.len() - self.moof_len;
        if self.mdat_header == 8 {
            let size = u32::try_from(mdat_len)
                .map_err(|_| Fmp4Error::Unsupported("an mdat size does not fit".into()))?;
            self.out[self.moof_len..self.moof_len + 4].copy_from_slice(&size.to_be_bytes());
        } else {
            self.out[self.moof_len + 8..self.moof_len + 16]
                .copy_from_slice(&(mdat_len as u64).to_be_bytes());
        }

        let tracks = parse_moof(&self.out[..self.moof_len], header_len, &self.tracks)?;
        // The kept fragment must say exactly what was kept, sample by sample.
        let declared = tracks
            .iter()
            .flat_map(|track| track.samples())
            .map(|sample| sample.size);
        if !declared.eq(self
            .spans
            .iter()
            .zip(&self.dropped)
            .map(|(span, dropped)| span.size - dropped))
        {
            return Err(Fmp4Error::Malformed(
                "a kept fragment does not declare what it keeps".into(),
            ));
        }
        Ok(TrimmedFragment {
            kept: Fragment {
                mdat_payload: payload_start..self.out.len(),
                bytes: self.out,
                tracks,
            },
            dropped: self.dropped,
        })
    }
}

/// Where the `moof`'s single `tfhd` carries `default_sample_size`, if it does.
fn default_sample_size_field(moof: &[u8], header_len: usize) -> Option<usize> {
    let body = moof.get(header_len..)?;
    let mut found = None;
    for (traf, traf_start, traf_end) in children(body).ok()? {
        if traf.kind() != b"traf" {
            continue;
        }
        let payload = &body[traf_start..traf_end];
        for (tfhd, start, end) in children(payload).ok()? {
            if tfhd.kind() != b"tfhd" {
                continue;
            }
            if found.is_some() || end - start < 8 {
                return None;
            }
            let flags = be_u32(payload, start) & 0x00ff_ffff;
            if flags & 0x00_0010 == 0 {
                return None;
            }
            let at = start
                + 8
                + 4 * usize::from(flags & 0x00_0002 != 0)
                + 4 * usize::from(flags & 0x00_0008 != 0);
            if at + 4 > end {
                return None;
            }
            found = Some(header_len + traf_start + at);
        }
    }
    found
}

#[cfg(test)]
#[path = "trim_tests.rs"]
mod tests;
