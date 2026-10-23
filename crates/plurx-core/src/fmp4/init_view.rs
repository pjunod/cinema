use super::{
    avc_rfc6381_codec_bytes, bounded_reader, hevc_parameter_sets_complete_bytes, parse_moov,
    peek_box_before, validate_hevc_sample_entries_bytes, Fmp4Error, FragmentReaderBounds,
    HevcSampleEntryLayout, Track,
};

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
        let mut bytes = boxed(b"ftyp", &[]);
        bytes.extend_from_slice(&boxed(b"moov", &moov));
        bytes.extend_from_slice(&boxed(b"free", &[]));
        bytes
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
}
