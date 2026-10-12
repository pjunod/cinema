//! Progressive scan evidence from the selected stream's HEVC configuration.
//! Missing field_order is common in HEVC probes. Require explicit progressive,
//! non-interlaced and frame-only PTL flags in both hvcC and every included SPS;
//! never infer progressive video merely from a codec, profile or missing flag.

use serde_json::Value;

const MAX_EXTRADATA_BYTES: usize = 64 * 1024;

pub(super) fn progressive_configuration(stream: &Value) -> bool {
    stream.get("codec_name").and_then(Value::as_str) == Some("hevc")
        && stream
            .get("extradata")
            .and_then(Value::as_str)
            .and_then(parse_dump)
            .is_some_and(|bytes| progressive_hvcc(&bytes))
}

fn parse_dump(dump: &str) -> Option<Vec<u8>> {
    if dump.len() > MAX_EXTRADATA_BYTES * 6 {
        return None;
    }
    let mut bytes = Vec::new();
    for line in dump.lines().filter(|line| !line.trim().is_empty()) {
        let (offset, data) = line.split_once(':')?;
        if offset.len() != 8 || usize::from_str_radix(offset, 16).ok()? != bytes.len() {
            return None;
        }
        let hex = data.strip_prefix(' ')?.split_once("  ")?.0;
        let before = bytes.len();
        for word in hex.split_ascii_whitespace() {
            if !matches!(word.len(), 2 | 4) || !word.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            for pair in word.as_bytes().chunks_exact(2) {
                bytes.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
            }
        }
        if !(1..=16).contains(&(bytes.len() - before)) || bytes.len() > MAX_EXTRADATA_BYTES {
            return None;
        }
    }
    Some(bytes)
}

fn progressive_flags(flags: u8) -> bool {
    flags & 0xd0 == 0x90
}

fn progressive_parameter_set(nal: &[u8], kind: u8) -> bool {
    if nal.len() < 15 || nal[0] & 1 != 0 || nal[1] >> 3 != 0 || nal[1] & 7 == 0 {
        return false;
    }
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for (index, byte) in nal[2..].iter().copied().enumerate() {
        if zeros >= 2 && byte == 3 {
            if nal.get(index + 3).is_none_or(|next| *next > 3) {
                return false;
            }
            zeros = 0;
            continue;
        }
        rbsp.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    // SPS general PTL follows its one-byte prefix; VPS general PTL follows
    // its four-byte prefix. Reject a contradictory included VPS as well.
    let prefix = if kind == 33 { 1 } else { 4 };
    rbsp.len() >= prefix + 12
        && (kind != 33 || (rbsp[0] >> 1) & 7 <= 6)
        && progressive_flags(rbsp[prefix + 5])
}

fn progressive_hvcc(bytes: &[u8]) -> bool {
    if bytes.len() < 23
        || bytes[0] != 1
        || bytes[1] >> 6 != 0
        || !progressive_flags(bytes[6])
        || bytes[13] >> 4 != 15
        || bytes[15] >> 2 != 63
        || bytes[16] >> 2 != 63
        || bytes[17] >> 3 != 31
        || bytes[18] >> 3 != 31
        || bytes[22] == 0
    {
        return false;
    }
    let mut cursor = 23;
    let mut sps_seen = false;
    for _ in 0..bytes[22] {
        let Some(header) = bytes.get(cursor..cursor + 3) else {
            return false;
        };
        if header[0] & 0x40 != 0 {
            return false;
        }
        let kind = header[0] & 63;
        let count = u16::from_be_bytes([header[1], header[2]]);
        if count == 0 {
            return false;
        }
        cursor += 3;
        for _ in 0..count {
            let Some(length) = bytes.get(cursor..cursor + 2) else {
                return false;
            };
            let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
            cursor += 2;
            let Some(nal) = bytes.get(cursor..cursor + length) else {
                return false;
            };
            if length < 2 || nal[0] & 0x80 != 0 || (nal[0] >> 1) & 63 != kind {
                return false;
            }
            if matches!(kind, 32 | 33) && !progressive_parameter_set(nal, kind) {
                return false;
            }
            sps_seen |= kind == 33;
            cursor += length;
        }
    }
    sps_seen && cursor == bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Vec<u8> {
        let document: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/hevc-progressive-configuration.json"
        ))
        .expect("captured HEVC document");
        parse_dump(
            document["streams"][0]["extradata"]
                .as_str()
                .expect("captured extradata dump"),
        )
        .expect("valid captured extradata dump")
    }
    #[test]
    fn captured_main10_record_proves_progressive_frame_only_video() {
        assert!(progressive_hvcc(&record()));
    }
    #[test]
    fn every_parameter_set_and_the_configuration_must_agree_on_scan_flags() {
        for index in [6, 40, 75] {
            // hvcC, VPS, SPS PTL in the captured record.
            for (clear, set) in [(0x80, 0), (0, 0x40), (0x10, 0)] {
                let mut bytes = record();
                bytes[index] = (bytes[index] & !clear) | set;
                assert!(!progressive_hvcc(&bytes), "conflicting flags at {index}");
            }
        }
    }
    #[test]
    fn malformed_incomplete_and_non_hvcc_records_do_not_prove_scan_type() {
        let original = record();
        for length in 0..original.len() {
            assert!(!progressive_hvcc(&original[..length]));
        }
        for (index, value) in [(0, 0), (13, 0), (22, 0), (22, 255), (23, 0x61)] {
            let mut bytes = original.clone();
            bytes[index] = value;
            assert!(!progressive_hvcc(&bytes));
        }
        let mut trailing = original;
        trailing.push(0);
        assert!(!progressive_hvcc(&trailing));
    }
    #[test]
    fn dump_offsets_and_hex_bytes_are_bounded_and_ascii_is_not_evidence() {
        assert_eq!(
            parse_dump("00000000: 0102 b000  hevc 0102\n"),
            Some(vec![1, 2, 0xb0, 0])
        );
        for dump in [
            "00000001: 0102  ..",
            "00000000: zz  ..",
            "00000000: 010  ..",
            "00000000: 0102  ..\n00000001: 03  .",
            "00000000: 0102",
        ] {
            assert!(parse_dump(dump).is_none());
        }
        assert!(parse_dump(&"x".repeat(MAX_EXTRADATA_BYTES * 6 + 1)).is_none());
    }
}
