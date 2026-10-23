use super::{
    avc_rfc6381_codec_bytes, bounded_reader, child_iter::iter_children,
    hevc_parameter_sets_complete_bytes, parse_moov, peek_box_before,
    validate_hevc_sample_entries_bytes, BoxHeader, Fmp4Error, FragmentReaderBounds,
    HevcSampleEntryLayout, Track,
};
use std::cell::Cell;

const BORROWED_INIT_MAX_METADATA_BOXES: usize = 1_024;
const BORROWED_INIT_MAX_SAMPLE_DESCRIPTIONS: usize = 16;

/// Parsed initialization metadata that borrows the original init bytes.
///
/// Track metadata is owned and separately allocated. This avoids the reader
/// input buffer and `Init.bytes` copy, but is not a total parser-memory bound.
#[derive(Debug)]
pub struct BorrowedInit<'a> {
    bytes: &'a [u8],
    tracks: Vec<Track>,
}

impl<'a> BorrowedInit<'a> {
    /// Maximum relevant child boxes traversed while parsing one borrowed init.
    pub const MAX_METADATA_BOXES: usize = BORROWED_INIT_MAX_METADATA_BOXES;
    /// Maximum sample-description boxes, including unsupported/unknown entries.
    pub const MAX_SAMPLE_DESCRIPTIONS: usize = BORROWED_INIT_MAX_SAMPLE_DESCRIPTIONS;

    /// Requested metadata-vector allocation ceiling for one sequential parse
    /// plus the codec inspection methods on this view. Preflight enforces the
    /// counts before track allocation. Payload, returned codec strings, stack,
    /// allocator-private headers and HTTP framework state are separate.
    pub fn metadata_workspace_bytes(bounds: FragmentReaderBounds) -> Result<usize, Fmp4Error> {
        use std::mem::size_of;
        bounds.validate()?;
        let tracks = bounds.max_tracks.min(Self::MAX_METADATA_BOXES);
        // Sum every relevant vector site, even sites whose lifetimes do not
        // overlap: child ranges; moov/trak lookup plus four HEVC kind lookups;
        // returned locations; merged HEVC entries; tracks, trex and NAL types.
        // Location ancestor chains are fixed inline arrays, not hidden Vecs.
        let ranges = size_of::<super::Child>()
            .checked_add(
                size_of::<(super::BoxAt, super::BoxHeader)>()
                    .checked_mul(5)
                    .ok_or_else(workspace_overflow)?,
            )
            .and_then(|n| n.checked_mul(Self::MAX_METADATA_BOXES))
            .ok_or_else(workspace_overflow)?;
        let locations = size_of::<super::AvcCLocation>()
            .checked_add(size_of::<Option<super::HvcCLocation>>())
            .and_then(|n| n.checked_add(size_of::<(super::BoxAt, super::BoxHeader)>()))
            .and_then(|n| n.checked_mul(Self::MAX_SAMPLE_DESCRIPTIONS))
            .ok_or_else(workspace_overflow)?;
        let track_tables = size_of::<Track>()
            .checked_add(size_of::<(u32, u32, u32, u32)>())
            .and_then(|n| n.checked_mul(tracks))
            .ok_or_else(workspace_overflow)?;
        ranges
            .checked_add(locations)
            .and_then(|n| n.checked_add(track_tables))
            .and_then(|n| n.checked_add(64))
            .ok_or_else(workspace_overflow)
    }

    /// Parse the first `ftyp`/`moov` initialization span from a complete input.
    /// Later bytes are left untouched, matching `FragmentReader`'s first-unit
    /// behavior. The input and init span must fit the supplied byte/unit bounds.
    pub fn parse(bytes: &'a [u8], bounds: FragmentReaderBounds) -> Result<Self, Fmp4Error> {
        bounds.validate()?;
        if bytes.len() > bounds.unit_bytes || bytes.len() > bounds.buffer_bytes {
            return Err(Fmp4Error::Unsupported(
                "borrowed fMP4 initialization exceeds bounded input size".into(),
            ));
        }

        let mut pos = 0usize;
        let mut init_start = None;
        while pos < bytes.len() {
            let header = peek_box_before(bytes, pos, Some(bytes.len()))?
                .ok_or_else(|| Fmp4Error::Malformed("truncated top-level fMP4 box".into()))?;
            bounded_reader::check_box_size(&header, &bounds)?;
            let end = pos
                .checked_add(header.size)
                .ok_or_else(|| Fmp4Error::Malformed("fMP4 box end overflows".into()))?;

            if header.kind() == b"ftyp" && init_start.is_none() {
                init_start = Some(pos);
            }
            if header.kind() == b"moov" {
                let start = init_start.unwrap_or(pos);
                bounded_reader::check_unit_size(pos - start, header.size, &bounds)?;
                let payload_start = pos + header.header_len;
                bounded_reader::preflight_moov(&bytes[payload_start..end], &bounds)?;
                preflight_moov_shape(&bytes[payload_start..end])?;
                let tracks = parse_moov(&bytes[payload_start..end])?;
                return Ok(Self {
                    bytes: &bytes[start..end],
                    tracks,
                });
            }
            pos = end;
        }

        Err(Fmp4Error::Malformed(
            "fMP4 input has no complete initialization segment".into(),
        ))
    }

    /// The original borrowed `ftyp` through `moov` span.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn video(&self) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|track| track.kind == super::TrackKind::Video)
    }

    pub fn avc_rfc6381_codec(&self) -> Result<Option<String>, Fmp4Error> {
        avc_rfc6381_codec_bytes(self.bytes, self.video())
    }

    pub fn hevc_parameter_sets_complete(&self) -> Result<bool, Fmp4Error> {
        hevc_parameter_sets_complete_bytes(self.bytes, self.video())
    }

    pub fn validate_hevc_sample_entries(&self) -> Result<HevcSampleEntryLayout, Fmp4Error> {
        validate_hevc_sample_entries_bytes(self.bytes, self.video())
    }
}

fn add_metadata_box(count: &Cell<usize>) -> Result<(), Fmp4Error> {
    let next = count
        .get()
        .checked_add(1)
        .ok_or_else(|| Fmp4Error::Unsupported("fMP4 metadata box count overflows".into()))?;
    if next > BORROWED_INIT_MAX_METADATA_BOXES {
        return Err(Fmp4Error::Unsupported(
            "borrowed fMP4 initialization has too many metadata boxes".into(),
        ));
    }
    count.set(next);
    Ok(())
}

fn visit_children(
    payload: &[u8],
    boxes: &Cell<usize>,
    mut visit: impl FnMut(BoxHeader, usize, usize) -> Result<(), Fmp4Error>,
) -> Result<(), Fmp4Error> {
    for child in iter_children(payload) {
        let (header, start, end) = child?;
        add_metadata_box(boxes)?;
        visit(header, start, end)?;
    }
    Ok(())
}

fn preflight_moov_shape(payload: &[u8]) -> Result<(), Fmp4Error> {
    let boxes = Cell::new(0usize);
    let descriptions = Cell::new(0usize);
    visit_children(payload, &boxes, |header, start, end| {
        if header.kind() == b"trak" {
            preflight_trak_shape(&payload[start..end], &boxes, &descriptions)?;
        } else if header.kind() == b"mvex" {
            visit_children(&payload[start..end], &boxes, |_, _, _| Ok(()))?;
        }
        Ok(())
    })
}

fn preflight_trak_shape(
    payload: &[u8],
    boxes: &Cell<usize>,
    descriptions: &Cell<usize>,
) -> Result<(), Fmp4Error> {
    visit_children(payload, boxes, |header, start, end| {
        if header.kind() == b"mdia" {
            preflight_mdia_shape(&payload[start..end], boxes, descriptions)?;
        }
        Ok(())
    })
}

fn preflight_mdia_shape(
    payload: &[u8],
    boxes: &Cell<usize>,
    descriptions: &Cell<usize>,
) -> Result<(), Fmp4Error> {
    visit_children(payload, boxes, |header, start, end| {
        if header.kind() == b"minf" {
            let minf = &payload[start..end];
            visit_children(minf, boxes, |minf_header, minf_start, minf_end| {
                if minf_header.kind() == b"stbl" {
                    preflight_stbl_shape(&minf[minf_start..minf_end], boxes, descriptions)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    })
}

fn preflight_stbl_shape(
    payload: &[u8],
    boxes: &Cell<usize>,
    descriptions: &Cell<usize>,
) -> Result<(), Fmp4Error> {
    visit_children(payload, boxes, |header, start, end| {
        if header.kind() != b"stsd" {
            return Ok(());
        }
        let stsd = &payload[start..end];
        if stsd.len() < 8 {
            return Err(Fmp4Error::Malformed("stsd too short".into()));
        }
        let entries = &stsd[8..];
        visit_children(entries, boxes, |entry, entry_start, entry_end| {
            let next = descriptions.get().checked_add(1).ok_or_else(|| {
                Fmp4Error::Unsupported("fMP4 sample-description count overflows".into())
            })?;
            if next > BORROWED_INIT_MAX_SAMPLE_DESCRIPTIONS {
                return Err(Fmp4Error::Unsupported(
                    "borrowed fMP4 initialization has too many sample descriptions".into(),
                ));
            }
            descriptions.set(next);
            if matches!(
                entry.kind(),
                b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" | b"avc1" | b"avc3" | b"dva1" | b"dvav"
            ) {
                let body = &entries[entry_start..entry_end];
                if body.len() < 78 {
                    return Err(Fmp4Error::Malformed("visual sample entry too short".into()));
                }
                visit_children(&body[78..], boxes, |_, _, _| Ok(()))?;
            }
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + payload.len());
        bytes.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn track(id: u32) -> Vec<u8> {
        let mut tkhd = [0u8; 16];
        tkhd[12..16].copy_from_slice(&id.to_be_bytes());
        boxed(b"trak", &boxed(b"tkhd", &tkhd))
    }

    fn init(track_count: usize) -> Vec<u8> {
        let mut moov = Vec::new();
        for id in 1..=track_count as u32 {
            moov.extend_from_slice(&track(id));
        }
        init_with_moov(&moov)
    }

    fn init_with_moov(moov: &[u8]) -> Vec<u8> {
        let mut bytes = boxed(b"ftyp", &[]);
        bytes.extend_from_slice(&boxed(b"moov", moov));
        bytes.extend_from_slice(&boxed(b"free", &[]));
        bytes
    }

    fn track_with_descriptions(count: usize) -> Vec<u8> {
        let mut descriptions = Vec::new();
        for _ in 0..count {
            descriptions.extend_from_slice(&boxed(b"junk", &[]));
        }
        let mut stsd = [0u8; 8].to_vec();
        stsd[4..8].copy_from_slice(&(count as u32).to_be_bytes());
        stsd.extend_from_slice(&descriptions);
        let hierarchy = boxed(
            b"mdia",
            &boxed(b"minf", &boxed(b"stbl", &boxed(b"stsd", &stsd))),
        );
        let mut payload = Vec::new();
        let mut tkhd = [0u8; 16];
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&boxed(b"tkhd", &tkhd));
        payload.extend_from_slice(&hierarchy);
        boxed(b"trak", &payload)
    }

    fn bounds(unit_bytes: usize) -> FragmentReaderBounds {
        FragmentReaderBounds {
            buffer_bytes: unit_bytes,
            unit_bytes,
            max_tracks: 1,
            max_runs: 1,
            max_samples: 1,
        }
    }

    #[test]
    fn borrowed_init_refuses_truncated_and_over_limit_input() {
        let truncated = [0, 0, 0, 16, b'f', b't', b'y', b'p'];
        assert!(matches!(
            BorrowedInit::parse(&truncated, bounds(32)),
            Err(Fmp4Error::Malformed(_))
        ));
        let too_large = [0; 33];
        assert!(matches!(
            BorrowedInit::parse(&too_large, bounds(32)),
            Err(Fmp4Error::Unsupported(_))
        ));
    }

    #[test]
    fn borrowed_init_returns_original_first_init_span() {
        let input = init(1);
        let parsed = BorrowedInit::parse(&input, bounds(128)).expect("bounded init");
        assert_eq!(parsed.bytes().as_ptr(), input.as_ptr());
        assert_eq!(parsed.bytes().len(), input.len() - 8);
        assert_eq!(parsed.tracks().len(), 1);
    }

    #[test]
    fn borrowed_init_preflights_track_count_before_metadata_parse() {
        let input = init(2);
        assert!(matches!(
            BorrowedInit::parse(&input, bounds(128)),
            Err(Fmp4Error::Unsupported(_))
        ));
    }

    #[test]
    fn borrowed_init_preflights_total_relevant_box_count() {
        let mut moov = track(1);
        for _ in 0..BORROWED_INIT_MAX_METADATA_BOXES {
            moov.extend_from_slice(&boxed(b"junk", &[]));
        }
        let input = init_with_moov(&moov);
        assert!(matches!(
            BorrowedInit::parse(&input, bounds(input.len())),
            Err(Fmp4Error::Unsupported(_))
        ));
    }

    #[test]
    fn borrowed_init_preflights_unknown_sample_descriptions() {
        let moov = track_with_descriptions(BORROWED_INIT_MAX_SAMPLE_DESCRIPTIONS + 1);
        let input = init_with_moov(&moov);
        assert!(matches!(
            BorrowedInit::parse(&input, bounds(input.len())),
            Err(Fmp4Error::Unsupported(_))
        ));
    }
}

fn workspace_overflow() -> Fmp4Error {
    Fmp4Error::Unsupported("borrowed init metadata workspace overflow".into())
}
