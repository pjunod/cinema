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

use libfuzzer_sys::fuzz_target;
use plurx_core::transcode::dvconvert::convert_length_prefixed;

/// A disc's largest video sample is well under this; the converter allocates
/// in proportion to its input, so the bound is on campaign throughput.
const MAX_FUZZ_BYTES: usize = 1024 * 1024;

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
            // The rewrite drops metadata and never grows a sample: an output
            // longer than its input means a prefix was written wrong.
            assert!(
                out.len() <= sample.len(),
                "converted sample grew from {} to {} bytes",
                sample.len(),
                out.len()
            );
            let _ = report;
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
