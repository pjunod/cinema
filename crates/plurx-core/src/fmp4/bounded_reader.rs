//! Explicit accumulation and parser-shape limits for a finite fMP4 feed.
//!
//! Returned `Init` and `Fragment` units own copied payload bytes, and parsed
//! tracks, runs, and samples own separate allocations. A caller retaining units
//! must budget those results separately from this reader's input buffer.

use super::{
    be_u32, malformed, peek_box_before, BoxHeader, Fmp4Error, FragmentReader, Unit, MAX_BOX_BYTES,
};

/// Limits for a single retained input buffer, unit, and parsed metadata shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentReaderBounds {
    pub buffer_bytes: usize,
    pub unit_bytes: usize,
    pub max_tracks: usize,
    pub max_runs: usize,
    pub max_samples: usize,
}

impl FragmentReaderBounds {
    pub fn validate(&self) -> Result<(), Fmp4Error> {
        if self.buffer_bytes == 0
            || self.unit_bytes < 8
            || self.max_tracks == 0
            || self.max_runs == 0
            || self.max_samples == 0
            || self.unit_bytes > self.buffer_bytes
            || self.buffer_bytes > MAX_BOX_BYTES
        {
            return Err(Fmp4Error::Unsupported(
                "invalid bounded fMP4 reader limits".into(),
            ));
        }
        Ok(())
    }
}

/// A reader whose input allocation is reserved once and never grown.
///
/// Construct this before registering or entering a native producer task, so
/// an unavailable buffer budget cannot leave a producer running. This bounds
/// input accumulation and parsed track/run/sample counts; it is not a total
/// process memory quota. Returned units and downstream copies need their own
/// budgets.
#[derive(Debug)]
pub struct BoundedFragmentReader {
    inner: FragmentReader,
    bounds: FragmentReaderBounds,
    failed: Option<Fmp4Error>,
}

impl BoundedFragmentReader {
    pub fn new(bounds: FragmentReaderBounds) -> Result<Self, Fmp4Error> {
        bounds.validate()?;
        let mut inner = FragmentReader::new();
        inner.reject_reinitialization();
        inner
            .buf
            .try_reserve_exact(bounds.buffer_bytes)
            .map_err(|_| Fmp4Error::Unsupported("bounded fMP4 input reservation failed".into()))?;
        if inner.buf.capacity() > bounds.buffer_bytes {
            return Err(Fmp4Error::Unsupported(
                "bounded fMP4 input reservation exceeded its limit".into(),
            ));
        }
        Ok(Self {
            inner,
            bounds,
            failed: None,
        })
    }

    /// Append atomically. Overflow is sticky because discarded bytes cannot
    /// later be recovered by continuing the same parser state.
    pub fn try_push(&mut self, data: &[u8]) -> Result<(), Fmp4Error> {
        if let Some(err) = &self.failed {
            return Err(err.clone());
        }
        if self.inner.init_start.is_none() && self.inner.pos > 0 {
            self.inner.compact();
        }
        if data.len() > self.remaining_capacity() {
            let err = Fmp4Error::Malformed("bounded fMP4 input buffer overflow".into());
            self.failed = Some(err.clone());
            return Err(err);
        }
        self.inner.buf.extend_from_slice(data);
        Ok(())
    }

    pub fn next_unit(&mut self) -> Result<Option<Unit>, Fmp4Error> {
        if let Some(err) = &self.failed {
            return Err(err.clone());
        }
        match self.inner.next_unit_with_bounds(Some(&self.bounds)) {
            Ok(unit) => Ok(unit),
            Err(err) => {
                self.failed = Some(err.clone());
                Err(err)
            }
        }
    }

    pub fn buffered(&self) -> usize {
        self.inner.buffered()
    }

    pub fn remaining_capacity(&self) -> usize {
        let reclaimable = if self.inner.init_start.is_none() {
            self.inner.pos
        } else {
            0
        };
        self.bounds.buffer_bytes - (self.inner.buf.len() - reclaimable)
    }

    pub fn saw_trailer(&self) -> bool {
        self.inner.saw_trailer()
    }
}

pub(super) fn check_box_size(
    hdr: &BoxHeader,
    bounds: &FragmentReaderBounds,
) -> Result<(), Fmp4Error> {
    if hdr.size > bounds.unit_bytes {
        return Err(Fmp4Error::Unsupported(format!(
            "box {} exceeds bounded fMP4 unit size",
            super::fourcc(hdr.kind()),
        )));
    }
    Ok(())
}

pub(super) fn check_unit_size(
    first: usize,
    second: usize,
    bounds: &FragmentReaderBounds,
) -> Result<(), Fmp4Error> {
    if first
        .checked_add(second)
        .is_none_or(|size| size > bounds.unit_bytes)
    {
        return Err(Fmp4Error::Unsupported(
            "fMP4 unit exceeds bounded reader unit size".into(),
        ));
    }
    Ok(())
}

/// Visit complete children without building the `children()` vector used by
/// the ordinary parser. An incomplete extended header is malformed here.
fn walk_children(
    payload: &[u8],
    mut visit: impl FnMut(&BoxHeader, &[u8]) -> Result<(), Fmp4Error>,
) -> Result<(), Fmp4Error> {
    let mut pos = 0usize;
    while pos < payload.len() {
        if payload.len() - pos < 8 {
            return malformed("truncated fMP4 child header");
        }
        let hdr = peek_box_before(payload, pos, Some(payload.len()))?
            .ok_or_else(|| Fmp4Error::Malformed("truncated fMP4 extended child header".into()))?;
        let end = pos
            .checked_add(hdr.size)
            .ok_or_else(|| Fmp4Error::Malformed("fMP4 child size overflows".into()))?;
        visit(&hdr, &payload[pos + hdr.header_len..end])?;
        pos = end;
    }
    Ok(())
}

fn increment(count: &mut usize, limit: usize, name: &str) -> Result<(), Fmp4Error> {
    *count = count
        .checked_add(1)
        .ok_or_else(|| Fmp4Error::Malformed(format!("fMP4 {name} count overflows")))?;
    if *count > limit {
        return Err(Fmp4Error::Unsupported(format!(
            "fMP4 {name} count exceeds bounded reader limit",
        )));
    }
    Ok(())
}

pub(super) fn preflight_moov(
    payload: &[u8],
    bounds: &FragmentReaderBounds,
) -> Result<(), Fmp4Error> {
    let mut tracks = 0;
    let mut trex = 0;
    walk_children(payload, |hdr, body| {
        match hdr.kind() {
            b"trak" => increment(&mut tracks, bounds.max_tracks, "track")?,
            b"mvex" => walk_children(body, |child, _| {
                if child.kind() == b"trex" {
                    increment(&mut trex, bounds.max_tracks, "trex")?;
                }
                Ok(())
            })?,
            _ => {}
        }
        Ok(())
    })
}

pub(super) fn preflight_moof(
    payload: &[u8],
    bounds: &FragmentReaderBounds,
) -> Result<(), Fmp4Error> {
    let mut tracks = 0;
    let mut runs = 0;
    let mut samples = 0usize;
    walk_children(payload, |hdr, body| {
        if hdr.kind() != b"traf" {
            return Ok(());
        }
        increment(&mut tracks, bounds.max_tracks, "track fragment")?;
        walk_children(body, |child, trun| {
            if child.kind() == b"trun" {
                increment(&mut runs, bounds.max_runs, "run")?;
                if trun.len() < 8 {
                    return malformed("trun too short");
                }
                samples = samples
                    .checked_add(be_u32(trun, 4) as usize)
                    .ok_or_else(|| Fmp4Error::Malformed("fMP4 sample count overflows".into()))?;
                if samples > bounds.max_samples {
                    return Err(Fmp4Error::Unsupported(
                        "fMP4 sample count exceeds bounded reader limit".into(),
                    ));
                }
            }
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> FragmentReaderBounds {
        FragmentReaderBounds {
            buffer_bytes: 128,
            unit_bytes: 96,
            max_tracks: 1,
            max_runs: 2,
            max_samples: 3,
        }
    }

    #[test]
    fn declared_oversized_header_is_refused_before_payload() {
        let mut reader = BoundedFragmentReader::new(bounds()).unwrap();
        reader.try_push(&256u32.to_be_bytes()).unwrap();
        reader.try_push(b"ftyp").unwrap();
        assert!(matches!(reader.next_unit(), Err(Fmp4Error::Unsupported(_))));
        assert_eq!(reader.buffered(), 8);
    }

    #[test]
    fn overflow_preserves_capacity_and_latches_failure() {
        let mut reader = BoundedFragmentReader::new(bounds()).unwrap();
        let capacity = reader.inner.buf.capacity();
        reader.try_push(&[0; 120]).unwrap();
        let first = reader.try_push(&[0; 9]).unwrap_err();
        assert_eq!(reader.inner.buf.capacity(), capacity);
        assert_eq!(reader.buffered(), 120);
        assert_eq!(reader.try_push(&[1]), Err(first.clone()));
        assert_eq!(reader.next_unit(), Err(first));
    }

    #[test]
    fn aggregate_default_only_truns_refused_before_parse() {
        let mut traf = Vec::new();
        for _ in 0..2 {
            traf.extend_from_slice(&16u32.to_be_bytes());
            traf.extend_from_slice(b"trun");
            traf.extend_from_slice(&0u32.to_be_bytes());
            traf.extend_from_slice(&2u32.to_be_bytes());
        }
        let mut moof = Vec::new();
        moof.extend_from_slice(&(traf.len() as u32 + 16).to_be_bytes());
        moof.extend_from_slice(b"moof");
        moof.extend_from_slice(&(traf.len() as u32 + 8).to_be_bytes());
        moof.extend_from_slice(b"traf");
        moof.extend_from_slice(&traf);
        let mut reader = BoundedFragmentReader::new(bounds()).unwrap();
        reader.inner.init_done = true;
        reader.try_push(&moof).unwrap();
        assert!(matches!(reader.next_unit(), Err(Fmp4Error::Unsupported(_))));
    }

    #[test]
    fn exact_unit_limit_accepts_complete_box() {
        let mut reader = BoundedFragmentReader::new(FragmentReaderBounds {
            buffer_bytes: 128,
            unit_bytes: 96,
            ..bounds()
        })
        .unwrap();
        let mut box_bytes = vec![0; 96];
        box_bytes[..4].copy_from_slice(&96u32.to_be_bytes());
        box_bytes[4..8].copy_from_slice(b"mfra");
        reader.inner.init_done = true;
        reader.try_push(&box_bytes).unwrap();
        assert_eq!(reader.next_unit(), Ok(Some(Unit::Trailer)));
        assert!(reader.saw_trailer());
    }
}
