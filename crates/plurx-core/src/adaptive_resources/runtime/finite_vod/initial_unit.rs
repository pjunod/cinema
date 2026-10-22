//! Read exactly one fMP4 initialization unit from an owned producer pipe.
//!
//! The fixed scratch buffer, bounded reader input, and returned `Init` copy
//! are separate allocations. The caller budgets retained units and subsequent
//! fragment processing separately; this helper is not a process memory quota.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::fmp4::{BoundedFragmentReader, FragmentReaderBounds, Init, Unit};

const MAX_PRE_INIT_BOXES: usize = 32;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn parser_error(error: crate::fmp4::Fmp4Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// Consume complete top-level boxes through the first init, leaving the source
/// precisely at its end for replay or continued fragment reading. Each header
/// is presented to the bounded parser before its advertised body is awaited.
pub(super) async fn read_initial_unit<R: AsyncRead + Unpin>(
    source: &mut R,
    bounds: FragmentReaderBounds,
    read_chunk_bytes: usize,
) -> io::Result<Init> {
    bounds
        .validate()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid fMP4 reader bounds"))?;
    if read_chunk_bytes == 0 || read_chunk_bytes > bounds.buffer_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid initial-unit read chunk size",
        ));
    }

    let mut reader = BoundedFragmentReader::new(bounds).map_err(io::Error::other)?;
    let mut scratch = Vec::new();
    scratch.try_reserve_exact(read_chunk_bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            "initial-unit scratch reservation failed",
        )
    })?;
    if scratch.capacity() > read_chunk_bytes {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "initial-unit scratch reservation exceeded its limit",
        ));
    }
    scratch.resize(read_chunk_bytes, 0);

    for _ in 0..MAX_PRE_INIT_BOXES {
        let mut header = [0u8; 16];
        source.read_exact(&mut header[..8]).await?;
        let size32 = u32::from_be_bytes(header[..4].try_into().expect("four bytes"));
        let kind = &header[4..8];
        if kind == b"moof" || kind == b"mdat" || kind == b"mfra" {
            return Err(invalid("media or trailer box arrived before fMP4 init"));
        }
        if size32 == 0 {
            return Err(invalid("initial-unit box extends to unknown EOF"));
        }
        let (size, header_len) = if size32 == 1 {
            source.read_exact(&mut header[8..16]).await?;
            (
                u64::from_be_bytes(header[8..16].try_into().expect("eight bytes")),
                16usize,
            )
        } else {
            (u64::from(size32), 8usize)
        };
        if size < header_len as u64 {
            return Err(invalid("initial-unit box is shorter than its header"));
        }
        let size = usize::try_from(size)
            .map_err(|_| invalid("initial-unit box size cannot fit memory address space"))?;

        reader
            .try_push(&header[..header_len])
            .map_err(parser_error)?;
        match reader.next_unit().map_err(parser_error)? {
            Some(Unit::Init(init)) => return Ok(init),
            Some(Unit::Fragment(_) | Unit::Trailer) => {
                return Err(invalid("fMP4 parser returned media before init"));
            }
            None => {}
        }

        let mut remaining = size - header_len;
        while remaining > 0 {
            let take = remaining
                .min(read_chunk_bytes)
                .min(reader.remaining_capacity());
            if take == 0 {
                return Err(invalid("bounded fMP4 reader has no room for declared box"));
            }
            source.read_exact(&mut scratch[..take]).await?;
            reader.try_push(&scratch[..take]).map_err(parser_error)?;
            remaining -= take;
        }
        match reader.next_unit().map_err(parser_error)? {
            Some(Unit::Init(init)) => return Ok(init),
            Some(Unit::Fragment(_) | Unit::Trailer) => {
                return Err(invalid("fMP4 parser returned media before init"));
            }
            None => {}
        }
    }
    Err(invalid("fMP4 init exceeded the pre-init box count"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn bounds() -> FragmentReaderBounds {
        FragmentReaderBounds {
            buffer_bytes: 4 * 1024 * 1024,
            unit_bytes: 4 * 1024 * 1024,
            max_tracks: 8,
            max_runs: 32,
            max_samples: 100_000,
        }
    }

    #[tokio::test]
    async fn oversized_declaration_is_refused_without_awaiting_its_body() {
        let mut stream = Cursor::new([0, 64, 0, 0, b'f', b't', b'y', b'p']);
        let mut small = bounds();
        small.unit_bytes = 64;
        let error = read_initial_unit(&mut stream, small, 32)
            .await
            .expect_err("declared size must fail at header");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(stream.position(), 8);
    }

    #[tokio::test]
    async fn media_before_init_is_rejected_at_header() {
        let mut stream = Cursor::new([0, 0, 0, 8, b'm', b'o', b'o', b'f']);
        let error = read_initial_unit(&mut stream, bounds(), 32)
            .await
            .expect_err("media cannot precede init");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(stream.position(), 8);
    }

    #[tokio::test]
    async fn real_fixture_stops_at_exact_init_boundary() {
        let feed = crate::testfixtures::pipe("h264");
        let mut reference = crate::fmp4::FragmentReader::new();
        reference.push(&feed);
        let Some(Unit::Init(expected)) = reference.next_unit().expect("reference init") else {
            panic!("fixture has no init");
        };
        let mut stream = Cursor::new(feed);
        let actual = read_initial_unit(&mut stream, bounds(), 4096)
            .await
            .expect("bounded init");
        assert_eq!(actual.bytes, expected.bytes);
        assert_eq!(stream.position(), expected.bytes.len() as u64);
    }
}
