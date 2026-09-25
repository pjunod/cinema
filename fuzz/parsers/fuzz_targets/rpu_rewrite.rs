#![no_main]

//! `convert_length_prefixed` — the Profile 7 → 8.1 Dolby Vision rewrite — over
//! an arbitrary length-prefixed sample.
//!
//! The converter reads NAL length prefixes it does not trust, finds the RPU
//! units among them, and rewrites each through `dolby_vision`'s parser and
//! writer. A sample that is not a sample, a prefix that overruns the buffer,
//! an RPU that is a few bytes short, or a CRC that no longer matches must all
//! come back as `DvConvertError`; a panic in this crate or in the `dolby_vision`
//! dependency is a finding. The first byte selects the `hvcC` length size,
//! including the illegal values the function refuses up front.
//!
//! A conversion that succeeds must hand back a sample of the same shape: the
//! output's length prefixes, at the same width, tile it exactly and frame as
//! many units as the input's did. Its length is not checked — a rewritten RPU
//! can grow when emulation-prevention bytes are re-escaped, and
//! `fmp4::rewrite_video_samples` carries signed deltas for exactly that. A
//! refused conversion must leave the output empty.

use libfuzzer_sys::fuzz_target;
use plurx_core::transcode::dvconvert::convert_length_prefixed;

/// A disc's largest video sample is well under this; the converter allocates
/// in proportion to its input, so the bound is on campaign throughput.
const MAX_FUZZ_BYTES: usize = 1024 * 1024;

/// The number of units `nal_length_size`-byte prefixes frame in `sample` when
/// they tile it exactly, or `None` when a prefix, or the unit it declares,
/// runs past the end. Any bytes after the last whole unit are read as one more
/// prefix, so a sample with trailing bytes is `None` too.
fn units(sample: &[u8], nal_length_size: usize) -> Option<usize> {
    let mut at = 0usize;
    let mut count = 0usize;
    while at < sample.len() {
        let body = at.checked_add(nal_length_size)?;
        let prefix = sample.get(at..body)?;
        let length = prefix
            .iter()
            .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte));
        at = body
            .checked_add(length)
            .filter(|end| *end <= sample.len())?;
        count += 1;
    }
    Some(count)
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FUZZ_BYTES || data.is_empty() {
        return;
    }
    let (selector, sample) = data.split_first().expect("non-empty input");
    // The three legal `hvcC` widths get most of the budget; one slot in
    // eight is an illegal width so the refusal path stays covered.
    let nal_length_size: u8 = match selector % 8 {
        0..=2 => 4,
        3..=4 => 2,
        5..=6 => 1,
        _ => *selector,
    };
    let mut out = Vec::new();
    match convert_length_prefixed(sample, nal_length_size, &mut out) {
        Ok(report) => {
            // Success means the width was legal and every input prefix was
            // walked, so the input tiles too; the output must be framed the
            // same way, one unit out for each unit in.
            let width = usize::from(nal_length_size);
            let expected = units(sample, width)
                .expect("a sample the converter accepted is tiled by its own prefixes");
            let written = units(&out, width).unwrap_or_else(|| {
                panic!(
                    "converted sample of {} bytes is not tiled by {width}-byte prefixes",
                    out.len()
                )
            });
            assert_eq!(
                written, expected,
                "conversion framed {written} units from an input of {expected}"
            );
            assert!(
                report.rpus <= expected as u64,
                "conversion reported {} RPUs in a sample of {expected} units",
                report.rpus
            );
        }
        Err(error) => {
            assert!(
                out.is_empty(),
                "a refused conversion left {} bytes in the output",
                out.len()
            );
            let _ = error;
        }
    }
});
