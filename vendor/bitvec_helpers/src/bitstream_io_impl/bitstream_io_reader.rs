use std::io;

use bitstream_io::{BigEndian, BitRead, BitReader, Integer};

pub struct BitstreamIoReader<R: io::Read + io::Seek> {
    bs: BitReader<R, BigEndian>,
    len: u64,
}

/// Convenience type for Vec<u8> inner buffer
pub type BsIoVecReader = BitstreamIoReader<io::Cursor<Vec<u8>>>;

/// Convenience type for &[u8] inner buffer
pub type BsIoSliceReader<'a> = BitstreamIoReader<io::Cursor<&'a [u8]>>;

impl<R> BitstreamIoReader<R>
where
    R: io::Read + io::Seek,
{
    pub fn new(read: R, len_bytes: u64) -> Self {
        Self {
            bs: BitReader::new(read),
            len: len_bytes * 8,
        }
    }

    #[inline(always)]
    pub fn read_bit(&mut self) -> io::Result<bool> {
        self.bs.read_bit()
    }

    #[inline(always)]
    pub fn read<const BITS: u32, I: Integer>(&mut self) -> io::Result<I> {
        self.bs.read::<BITS, I>()
    }

    #[inline(always)]
    pub fn read_var<I: Integer>(&mut self, bits: u32) -> io::Result<I> {
        self.bs.read_var(bits)
    }

    #[inline(always)]
    pub fn read_ue(&mut self) -> io::Result<u64> {
        self.bs.read_unary::<1>().and_then(|leading_zeroes| {
            // PLURX-PATCH 1: 64 or more leading zeroes is not an Exp-Golomb
            // code a u64 can hold; upstream shifted `1 << leading_zeroes`,
            // which overflows at 64 (a panic with overflow checks, a wrong
            // value without). Sixty-four zero bits in a row are only ever a
            // corrupted or hostile stream, so refuse them as data.
            if leading_zeroes >= 64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("ue(v) with {leading_zeroes} leading zero bits does not fit in 64 bits"),
                ));
            }
            if leading_zeroes > 0 {
                self.bs
                    .read_var::<u64>(leading_zeroes)
                    .map(|v| v + (1 << leading_zeroes) - 1)
            } else {
                Ok(0)
            }
        })
    }

    #[inline(always)]
    pub fn read_se(&mut self) -> io::Result<i64> {
        self.read_ue().and_then(|code_num| {
            // PLURX-PATCH 1: `code_num + 1` and the negation both overflow at
            // the top of the u64 range (`-(2^63 as i64)`), which the `read_ue`
            // bound above does not exclude for 63 leading zeroes. A checked
            // conversion refuses the value instead of wrapping or panicking.
            let m = code_num / 2 + code_num % 2;
            let magnitude = i64::try_from(m).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("se(v) code {code_num} does not fit in an i64"),
                )
            })?;
            Ok(if code_num % 2 == 0 { -magnitude } else { magnitude })
        })
    }

    #[inline(always)]
    pub fn read_bytes(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.bs.read_bytes(buf)
    }

    #[inline(always)]
    pub fn byte_aligned(&self) -> bool {
        self.bs.byte_aligned()
    }

    #[inline(always)]
    pub fn available(&mut self) -> io::Result<u64> {
        self.bs.position_in_bits().map(|pos| self.len - pos)
    }

    #[inline(always)]
    pub fn skip_n(&mut self, n: u32) -> io::Result<()> {
        self.available().and_then(|avail| {
            if n as u64 > avail {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "skip_n: out of bounds bits",
                ))
            } else {
                self.bs.skip(n)
            }
        })
    }

    #[inline(always)]
    pub fn position_in_bits(&mut self) -> io::Result<u64> {
        self.bs.position_in_bits()
    }

    pub fn replace_inner(&mut self, read: R, len_bytes: u64) {
        self.len = len_bytes * 8;
        self.bs = BitReader::new(read);
    }
}

impl BsIoVecReader {
    pub fn from_vec(buf: Vec<u8>) -> Self {
        let len = buf.len() as u64;
        let read = io::Cursor::new(buf);

        Self::new(read, len)
    }

    pub fn replace_vec(&mut self, buf: Vec<u8>) {
        let len = buf.len() as u64;
        self.replace_inner(io::Cursor::new(buf), len);
    }
}

impl<'a> BsIoSliceReader<'a> {
    pub fn from_slice(buf: &'a [u8]) -> Self {
        let len = buf.len() as u64;
        let read = io::Cursor::new(buf);

        Self::new(read, len)
    }

    pub fn replace_slice(&mut self, buf: &'a [u8]) {
        let len = buf.len() as u64;
        self.replace_inner(io::Cursor::new(buf), len);
    }
}

impl Default for BsIoVecReader {
    fn default() -> Self {
        Self::from_vec(Vec::new())
    }
}

impl Default for BsIoSliceReader<'_> {
    fn default() -> Self {
        Self::from_slice(&[])
    }
}

#[test]
fn read_var_validations() {
    let mut reader = BsIoSliceReader::from_slice(&[1]);
    assert!(reader.read_var::<u8>(9).is_err());
    assert!(reader.read_var::<u16>(4).is_ok());

    assert!(reader.read_var::<u8>(8).is_err());
    assert!(reader.read_var::<u8>(4).is_ok());
    assert!(reader.read_bit().is_err());
}

#[test]
fn skip_n_validations() {
    let mut reader = BsIoSliceReader::from_slice(&[1]);
    assert!(reader.skip_n(9).is_err());

    assert!(reader.skip_n(7).is_ok());
    assert!(reader.read_bit().is_ok());
    assert!(reader.read_bit().is_err());
}
