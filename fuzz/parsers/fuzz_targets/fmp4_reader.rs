#![no_main]

//! `FragmentReader` over an arbitrary byte stream, fed the way the pipe feeds
//! it: in pieces, with `next_unit` polled after every push.
//!
//! The reader's contract is that malformed input is an `Err` and a truncated
//! feed is a permanent `Ok(None)`; a panic, an abort, or an allocation that
//! outgrows the input by orders of magnitude is a finding. The first byte of
//! the input picks the push size so the streaming path — a box header split
//! across two pushes, an `mdat` that arrives over many — is exercised rather
//! than only the whole-buffer case.

use libfuzzer_sys::fuzz_target;
use plurx_core::fmp4::{avc_rfc6381_codec, FragmentReader, Init, Unit};

/// Well above any segment the session lets the reader hold, and small
/// enough that a campaign spends its budget on structure rather than on
/// copying.
const MAX_FUZZ_BYTES: usize = 4 * 1024 * 1024;

/// Every unit the reader can yield costs at least a box header, so this can
/// never be reached by a well-formed input of `MAX_FUZZ_BYTES`; it exists so a
/// reader that yielded units without consuming bytes would terminate as a
/// failure rather than hang the campaign.
const MAX_UNITS: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FUZZ_BYTES || data.is_empty() {
        return;
    }
    let (selector, stream) = data.split_first().expect("non-empty input");
    // 0 → the whole stream in one push; otherwise a push size from 1 byte to
    // 64 KiB, so both the one-shot and the drip-fed shapes are covered.
    let push = match selector {
        0 => stream.len().max(1),
        n => 1usize << (n % 17),
    };

    let mut reader = FragmentReader::new();
    let mut init: Option<Init> = None;
    let mut units = 0usize;
    for chunk in stream.chunks(push) {
        reader.push(chunk);
        loop {
            match reader.next_unit() {
                Ok(Some(unit)) => {
                    units += 1;
                    assert!(
                        units <= MAX_UNITS,
                        "the reader yielded units without consuming input"
                    );
                    match unit {
                        Unit::Init(parsed) => {
                            // A pure read of the sample descriptions; it must
                            // answer or refuse, never panic.
                            let _ = avc_rfc6381_codec(&parsed);
                            let _ = parsed.video();
                            init = Some(parsed);
                        }
                        Unit::Fragment(fragment) => {
                            let _ = fragment.len();
                            let _ = fragment.is_empty();
                            if let Some(init) = init.as_ref() {
                                let _ = fragment.video_duration(init);
                            }
                        }
                        _ => {}
                    }
                }
                // More bytes needed, or a truncated feed: both are `Ok(None)`.
                Ok(None) => break,
                // Malformed input is expected; the reader is done with it.
                Err(_) => return,
            }
        }
        let _ = reader.buffered();
    }
    let _ = reader.saw_trailer();
});
