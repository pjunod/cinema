//! Bounded independent observation of the encoder's actual SPS/HRD.
use super::EnhancementRateEnvelope;

type Result<T> = std::result::Result<T, &'static str>;
struct Bits {
    bytes: Vec<u8>,
    bit: usize,
}
impl Bits {
    fn new(nal: &[u8]) -> Result<Self> {
        if nal.len() < 3 || nal.len() > 65_536 {
            return Err("bounded enhancement SPS");
        }
        let mut bytes = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for byte in &nal[2..] {
            if zeros == 2 && *byte == 3 {
                zeros = 0;
                continue;
            }
            bytes.push(*byte);
            zeros = if *byte == 0 { zeros + 1 } else { 0 };
        }
        Ok(Self { bytes, bit: 0 })
    }
    fn read(&mut self, n: usize) -> Result<u64> {
        if n > 64
            || self
                .bit
                .checked_add(n)
                .is_none_or(|end| end > self.bytes.len() * 8)
        {
            return Err("truncated enhancement SPS");
        }
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | u64::from((self.bytes[self.bit / 8] >> (7 - self.bit % 8)) & 1);
            self.bit += 1;
        }
        Ok(v)
    }
    fn flag(&mut self) -> Result<bool> {
        Ok(self.read(1)? != 0)
    }
    fn ue(&mut self) -> Result<u64> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros > 31 {
                return Err("oversized enhancement SPS Golomb field");
            }
        }
        Ok((1_u64 << zeros) - 1 + self.read(zeros)?)
    }
    fn se(&mut self) -> Result<()> {
        self.ue().map(|_| ())
    }
}

fn ptl(b: &mut Bits, sublayers: usize, expected: EnhancementRateEnvelope) -> Result<()> {
    b.read(2)?;
    if !b.flag()? {
        return Err("enhancement encoder changed HEVC High tier");
    }
    if b.read(5)? != 2 {
        return Err("enhancement encoder changed Main10 profile");
    }
    b.read(32)?;
    b.read(48)?;
    if b.read(8)? != u64::from(expected.hevc_level_idc()) {
        return Err("enhancement encoder clamped or changed SPS level");
    }
    let mut profile = [false; 7];
    let mut level = [false; 7];
    for i in 0..sublayers {
        profile[i] = b.flag()?;
        level[i] = b.flag()?;
    }
    if sublayers > 0 {
        b.read((8 - sublayers) * 2)?;
    }
    for i in 0..sublayers {
        if profile[i] {
            b.read(64)?;
            b.read(24)?;
        }
        if level[i] {
            b.read(8)?;
        }
    }
    Ok(())
}
fn bounded(v: u64, max: usize) -> Result<usize> {
    usize::try_from(v)
        .ok()
        .filter(|v| *v <= max)
        .ok_or("unbounded enhancement SPS list")
}
fn hrd(b: &mut Bits, sublayers: usize, expected: EnhancementRateEnvelope) -> Result<bool> {
    let nal = b.flag()?;
    let vcl = b.flag()?;
    let mut subpic = false;
    let mut br_scale = 0;
    let mut cpb_scale = 0;
    if nal || vcl {
        subpic = b.flag()?;
        if subpic {
            b.read(8)?;
            b.read(5)?;
            b.read(1)?;
            b.read(5)?;
        }
        br_scale = b.read(4)?;
        cpb_scale = b.read(4)?;
        if subpic {
            b.read(4)?;
        }
        b.read(5)?;
        b.read(5)?;
        b.read(5)?;
    }
    for _ in 0..=sublayers {
        let fixed = b.flag()?;
        let within = fixed || b.flag()?;
        let low_delay = if within {
            b.ue()?;
            false
        } else {
            b.flag()?
        };
        let count = if low_delay {
            1
        } else {
            bounded(b.ue()?, 31)? + 1
        };
        for present in [nal, vcl] {
            if present {
                for _ in 0..count {
                    let br = (b.ue()? + 1)
                        .checked_shl((6 + br_scale) as u32)
                        .ok_or("enhancement HRD bitrate overflow")?;
                    let cpb = (b.ue()? + 1)
                        .checked_shl((4 + cpb_scale) as u32)
                        .ok_or("enhancement HRD CPB overflow")?;
                    if subpic {
                        b.ue()?;
                        b.ue()?;
                    }
                    b.read(1)?;
                    // HRD syntax quantizes the requested values. Permit one syntax
                    // quantum, never a lower coded-level clamp or arbitrary drift.
                    let wanted_br = expected.peak_bps();
                    let wanted_cpb = u64::from(expected.buffer_kbits()) * 1000;
                    if br.abs_diff(wanted_br) >= (1_u64 << (6 + br_scale))
                        || cpb.abs_diff(wanted_cpb) >= (1_u64 << (4 + cpb_scale))
                    {
                        return Err("enhancement encoder clamped or changed signalled HRD budget");
                    }
                }
            }
        }
    }
    Ok(nal || vcl)
}
fn sps(nal: &[u8], expected: EnhancementRateEnvelope) -> Result<bool> {
    let mut b = Bits::new(nal)?;
    b.read(4)?;
    let sublayers = bounded(b.read(3)?, 6)?;
    b.read(1)?;
    ptl(&mut b, sublayers, expected)?;
    b.ue()?;
    let chroma = b.ue()?;
    if chroma > 3 {
        return Err("enhancement SPS chroma");
    }
    if chroma == 3 {
        b.read(1)?;
    }
    b.ue()?;
    b.ue()?;
    if b.flag()? {
        for _ in 0..4 {
            b.ue()?;
        }
    }
    if b.ue()? != 2 || b.ue()? != 2 {
        return Err("enhancement SPS changed 10-bit coding");
    }
    let poc = bounded(b.ue()?, 12)? + 4;
    let ordering = b.flag()?;
    for _ in (if ordering { 0 } else { sublayers })..=sublayers {
        b.ue()?;
        b.ue()?;
        b.ue()?;
    }
    for _ in 0..6 {
        b.ue()?;
    }
    if b.flag()? && b.flag()? {
        for size in 0..4 {
            for _ in (0..6).step_by(if size == 3 { 3 } else { 1 }) {
                if !b.flag()? {
                    b.ue()?;
                } else {
                    if size > 1 {
                        b.se()?;
                    }
                    for _ in 0..64.min(1 << (4 + 2 * size)) {
                        b.se()?;
                    }
                }
            }
        }
    }
    b.read(1)?;
    b.read(1)?;
    if b.flag()? {
        b.read(8)?;
        b.ue()?;
        b.ue()?;
        b.read(1)?;
    }
    let sets = bounded(b.ue()?, 64)?;
    let mut deltas = Vec::with_capacity(sets);
    for i in 0..sets {
        let predicted = i != 0 && b.flag()?;
        let count = if predicted {
            b.read(1)?;
            b.ue()?;
            let previous = deltas[i - 1];
            let mut count = 0;
            for _ in 0..=previous {
                let used = b.flag()?;
                let delta = used || b.flag()?;
                count += usize::from(delta);
            }
            count
        } else {
            let negative = bounded(b.ue()?, 64)?;
            let positive = bounded(b.ue()?, 64)?;
            for _ in 0..negative + positive {
                b.ue()?;
                b.read(1)?;
            }
            negative + positive
        };
        deltas.push(count);
    }
    if b.flag()? {
        let count = bounded(b.ue()?, 32)?;
        for _ in 0..count {
            b.read(poc)?;
            b.read(1)?;
        }
    }
    b.read(1)?;
    b.read(1)?;
    if !b.flag()? {
        return Ok(false);
    }
    if b.flag()? && b.read(8)? == 255 {
        b.read(16)?;
        b.read(16)?;
    }
    if b.flag()? {
        b.read(1)?;
    }
    if b.flag()? {
        b.read(3)?;
        b.read(1)?;
        if b.flag()? {
            b.read(24)?;
        }
    }
    if b.flag()? {
        b.ue()?;
        b.ue()?;
    }
    b.read(3)?;
    if b.flag()? {
        for _ in 0..4 {
            b.ue()?;
        }
    }
    if !b.flag()? {
        return Ok(false);
    }
    b.read(32)?;
    b.read(32)?;
    if b.flag()? {
        b.ue()?;
    }
    if b.flag()? {
        hrd(&mut b, sublayers, expected)
    } else {
        Ok(false)
    }
}

fn hvcc_box(bytes: &[u8], depth: usize, found: &mut Vec<Vec<u8>>) -> Result<()> {
    if depth > 10 {
        return Err("enhancement init nesting bound");
    }
    let mut at = 0;
    while at < bytes.len() {
        let header = bytes
            .get(at..at + 8)
            .ok_or("truncated enhancement init box")?;
        let size = u32::from_be_bytes(header[..4].try_into().map_err(|_| "box size")?) as usize;
        let size = if size == 0 { bytes.len() - at } else { size };
        if size < 8 {
            return Err("unsupported enhancement init box size");
        }
        let body = bytes
            .get(at + 8..at.checked_add(size).ok_or("box overflow")?)
            .ok_or("enhancement init box extent")?;
        match &header[4..8] {
            b"hvcC" => found.push(body.to_vec()),
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => hvcc_box(body, depth + 1, found)?,
            b"stsd" => hvcc_box(body.get(8..).ok_or("enhancement stsd")?, depth + 1, found)?,
            b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => hvcc_box(
                body.get(78..).ok_or("enhancement HEVC sample entry")?,
                depth + 1,
                found,
            )?,
            _ => {}
        }
        at += size;
    }
    Ok(())
}

/// Returns whether the output actually signals HRD. Absence grants no HRD
/// proof; tier and level are still independently checked in hvcC and all SPS.
pub fn validate_encoded_enhancement_envelope(
    bytes: &[u8],
    expected: EnhancementRateEnvelope,
) -> Result<bool> {
    use crate::fmp4::{FragmentReader, Unit};
    let mut reader = FragmentReader::new();
    reader.push(bytes);
    let Some(Unit::Init(init)) = reader.next_unit().map_err(|_| "enhancement encoded init")? else {
        return Err("enhancement encoded init absent");
    };
    let mut records = Vec::new();
    hvcc_box(&init.bytes, 0, &mut records)?;
    if records.len() != 1 {
        return Err("enhancement requires one hvcC");
    }
    if let (Some(expected_level), Some(record)) = (
        expected.dv_level(),
        crate::fmp4::dolby_vision_record(&init).map_err(|_| "enhancement DV config")?,
    ) {
        if record.level != expected_level {
            return Err("authored Dolby Vision level differs from frozen envelope");
        }
    }
    let record = &records[0];
    if record.len() < 23
        || record.len() > 256 * 1024
        || record[0] != 1
        || record[1] & 0x20 == 0
        || record[1] & 31 != 2
        || record[12] != expected.hevc_level_idc()
    {
        return Err("enhancement encoder clamped or changed hvcC profile/tier/level");
    }
    let mut cursor = 23;
    let mut seen = 0;
    let mut seen_vps = 0;
    let mut observed_hrd = false;
    for _ in 0..record[22] {
        let header = record
            .get(cursor..cursor + 3)
            .ok_or("truncated enhancement hvcC array")?;
        let kind = header[0] & 63;
        let count = u16::from_be_bytes([header[1], header[2]]);
        if count == 0 || count > 16 {
            return Err("enhancement hvcC parameter-set count bound");
        }
        cursor += 3;
        for _ in 0..count {
            let size = record
                .get(cursor..cursor + 2)
                .ok_or("truncated enhancement hvcC NAL length")?;
            let size = usize::from(u16::from_be_bytes([size[0], size[1]]));
            cursor += 2;
            let nal = record
                .get(cursor..cursor + size)
                .ok_or("truncated enhancement hvcC NAL")?;
            cursor += size;
            if nal.len() < 3 || (nal[0] >> 1) & 63 != kind || nal[0] & 0x81 != 0 || nal[1] != 1 {
                return Err("enhancement hvcC single-layer NAL header");
            }
            if kind == 33 {
                observed_hrd |= sps(nal, expected)?;
                seen += 1;
            }
            if kind == 32 {
                let mut b = Bits::new(nal)?;
                b.read(6)?;
                if b.read(6)? != 0 {
                    return Err("enhancement VPS is not single-layer");
                }
                let sublayers = bounded(b.read(3)?, 6)?;
                b.read(17)?;
                ptl(&mut b, sublayers, expected)?;
                seen_vps += 1;
            }
        }
    }
    if seen == 0 || seen_vps == 0 || cursor != record.len() {
        return Err("enhancement hvcC VPS/SPS missing or trailing data");
    }
    Ok(observed_hrd)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Writer(Vec<bool>);
    impl Writer {
        fn bits(&mut self, value: u64, n: usize) {
            for i in (0..n).rev() {
                self.0.push(value & (1 << i) != 0);
            }
        }
        fn ue(&mut self, value: u64) {
            let v = value + 1;
            let n = (64 - v.leading_zeros()) as usize;
            self.bits(0, n - 1);
            self.bits(v, n);
        }
        fn nal(self) -> Vec<u8> {
            self.nal_kind(33)
        }
        fn nal_kind(mut self, kind: u8) -> Vec<u8> {
            self.bits(1, 1);
            while !self.0.len().is_multiple_of(8) {
                self.bits(0, 1);
            }
            let raw: Vec<u8> = self
                .0
                .chunks_exact(8)
                .map(|bits| bits.iter().fold(0, |v, b| (v << 1) | u8::from(*b)))
                .collect();
            let mut result = vec![kind << 1, 1];
            let mut zeros = 0;
            for byte in raw {
                if zeros == 2 && byte <= 3 {
                    result.push(3);
                    zeros = 0;
                }
                result.push(byte);
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
            result
        }
    }
    fn fixture(level: u8, hrd: Option<(u64, u64)>) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.bits(1, 8); // VPS0, no sublayers, temporal nesting
        w.bits(0, 2);
        w.bits(1, 1);
        w.bits(2, 5);
        w.bits(0, 32);
        w.bits(0x9000_0000_0000, 48);
        w.bits(u64::from(level), 8);
        w.ue(0);
        w.ue(1);
        w.ue(3840);
        w.ue(2160);
        w.bits(0, 1);
        w.ue(2);
        w.ue(2);
        w.ue(4);
        w.bits(0, 1);
        for _ in 0..3 {
            w.ue(0);
        }
        for _ in 0..6 {
            w.ue(0);
        }
        w.bits(0, 1);
        w.bits(1, 1);
        w.bits(1, 1);
        w.bits(0, 1);
        w.ue(0);
        w.bits(0, 1);
        w.bits(1, 1);
        w.bits(1, 1);
        if let Some((br, cpb)) = hrd {
            w.bits(1, 1); // VUI
            w.bits(0, 8); // aspect, overscan, signal, chromaloc, neutral/field/frame, window
            w.bits(1, 1);
            w.bits(1, 32);
            w.bits(24, 32);
            w.bits(0, 1);
            w.bits(1, 1);
            w.bits(1, 1);
            w.bits(0, 1);
            w.bits(0, 1);
            w.bits(0, 4);
            w.bits(0, 4);
            w.bits(0, 15);
            w.bits(1, 1);
            w.ue(0);
            w.ue(0);
            w.ue(br / 64 - 1);
            w.ue(cpb / 16 - 1);
            w.bits(0, 1);
        } else {
            w.bits(0, 1);
        }
        w.nal()
    }
    fn expected() -> EnhancementRateEnvelope {
        EnhancementRateEnvelope::resolve(
            90_000,
            super::super::EnhancementRateProvenance::HeldVideoStream,
            3840,
            2160,
            (24, 1),
            true,
            Default::default(),
        )
        .expect("envelope")
    }
    #[test]
    fn enhancement_sps_observes_absent_hrd_and_rejects_actual_level_clamp() {
        assert_eq!(sps(&fixture(156, None), expected()), Ok(false));
        assert!(sps(&fixture(150, None), expected()).is_err());
        let mut truncated = fixture(156, None);
        truncated.truncate(8);
        assert!(sps(&truncated, expected()).is_err());
    }
    #[test]
    fn enhancement_sps_checks_signalled_peak_and_cpb_independently() {
        assert_eq!(
            sps(&fixture(156, Some((135_000_000, 180_000_000))), expected()),
            Ok(true)
        );
        assert!(sps(&fixture(156, Some((130_000_000, 180_000_000))), expected()).is_err());
        assert!(sps(&fixture(156, Some((135_000_000, 160_000_000))), expected()).is_err());
    }
    fn vps(level: u8) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.bits(0x0c01ffff, 32); // VPS0, one layer/sublayer, nesting, reserved ones
        w.bits(0, 2);
        w.bits(1, 1);
        w.bits(2, 5);
        w.bits(0, 32);
        w.bits(0x9000_0000_0000, 48);
        w.bits(u64::from(level), 8);
        w.nal_kind(32)
    }
    fn hvcc(level: u8, arrays: &[(u8, Vec<Vec<u8>>)]) -> Vec<u8> {
        let mut r = vec![0; 23];
        r[0] = 1;
        r[1] = 0x22;
        r[12] = level;
        r[21] = 3;
        r[22] = arrays.len() as u8;
        for (kind, nals) in arrays {
            r.push(0x80 | kind);
            r.extend_from_slice(&(nals.len() as u16).to_be_bytes());
            for nal in nals {
                r.extend_from_slice(&(nal.len() as u16).to_be_bytes());
                r.extend_from_slice(nal);
            }
        }
        r
    }
    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }
    /// A complete bounded ftyp/moov/trak/mdia/minf/stbl/stsd/hvc1/hvcC
    /// path, so these tests exercise publication entry, not only the SPS helper.
    fn init(records: &[Vec<u8>], dv_level: Option<u8>) -> Vec<u8> {
        let mut entry = vec![0; 78];
        for record in records {
            entry.extend(boxed(b"hvcC", record));
        }
        if let Some(level) = dv_level {
            let mut cfg = vec![0; 24];
            cfg[0] = 1;
            cfg[2..4].copy_from_slice(&((8_u16 << 9) | (u16::from(level) << 3) | 5).to_be_bytes());
            cfg[4] = 0x10;
            entry.extend(boxed(b"dvcC", &cfg));
        }
        let mut stsd = vec![0; 8];
        stsd[7] = 1;
        stsd.extend(boxed(b"hvc1", &entry));
        let mut mdia = boxed(b"minf", &boxed(b"stbl", &boxed(b"stsd", &stsd)));
        let mut hdlr = vec![0; 12];
        hdlr[8..12].copy_from_slice(b"vide");
        mdia.extend(boxed(b"hdlr", &hdlr));
        let mut tkhd = vec![0; 16];
        tkhd[15] = 1;
        let mut trak = boxed(b"tkhd", &tkhd);
        trak.extend(boxed(b"mdia", &mdia));
        let mut out = boxed(b"ftyp", b"isom\0\0\0\0isom");
        out.extend(boxed(b"moov", &boxed(b"trak", &trak)));
        out
    }
    #[test]
    fn enhancement_init_checks_hvcc_vps_sps_and_authored_dv_level_together() {
        let record = |header, vps_level, sps_level, hrd| {
            hvcc(
                header,
                &[
                    (32, vec![vps(vps_level)]),
                    (33, vec![fixture(sps_level, hrd)]),
                ],
            )
        };
        let valid = record(156, 156, 156, None);
        assert_eq!(
            validate_encoded_enhancement_envelope(
                &init(std::slice::from_ref(&valid), Some(10)),
                expected()
            ),
            Ok(false)
        );
        assert_eq!(
            validate_encoded_enhancement_envelope(
                &init(
                    &[record(156, 156, 156, Some((135_000_000, 180_000_000)))],
                    Some(10)
                ),
                expected()
            ),
            Ok(true)
        );
        for bytes in [
            init(&[record(153, 156, 156, None)], None),
            init(&[record(156, 153, 156, None)], None),
            init(&[record(156, 156, 153, None)], None),
            init(std::slice::from_ref(&valid), Some(6)),
            init(&[valid.clone(), valid.clone()], None),
            init(&[hvcc(156, &[(33, vec![fixture(156, None)])])], None),
            init(
                &[hvcc(
                    156,
                    &[
                        (32, vec![vps(156)]),
                        (33, vec![fixture(156, None), fixture(153, None)]),
                    ],
                )],
                None,
            ),
        ] {
            assert!(validate_encoded_enhancement_envelope(&bytes, expected()).is_err());
        }
        for header in [0x02, 0x21] {
            // Main tier or non-Main10 profile in hvcC
            let mut changed = valid.clone();
            changed[1] = header;
            assert!(
                validate_encoded_enhancement_envelope(&init(&[changed], None), expected()).is_err()
            );
        }
        for (kind, index) in [(32, 6), (33, 3)] {
            let mut bad = if kind == 32 {
                vps(156)
            } else {
                fixture(156, None)
            };
            bad[index] &= !0x20; // Profile/tier/level must agree in each NAL too.
            let arrays = if kind == 32 {
                vec![(32, vec![bad]), (33, vec![fixture(156, None)])]
            } else {
                vec![(32, vec![vps(156)]), (33, vec![bad])]
            };
            assert!(validate_encoded_enhancement_envelope(
                &init(&[hvcc(156, &arrays)], None),
                expected()
            )
            .is_err());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(
            validate_encoded_enhancement_envelope(&init(&[trailing], None), expected()).is_err()
        );
        let mut truncated = init(&[valid], None);
        truncated.pop();
        assert!(validate_encoded_enhancement_envelope(&truncated, expected()).is_err());
        let mut found = Vec::new();
        assert!(hvcc_box(&[0, 0, 0, 7, b'h', b'v', b'c', b'C'], 0, &mut found).is_err());
    }
}
