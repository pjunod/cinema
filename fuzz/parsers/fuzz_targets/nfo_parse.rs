#![no_main]

//! `scan::nfo::parse` over arbitrary text.
//!
//! The parser's contract is `None` for anything that is not a Kodi `<movie>`
//! sidecar and `Some` for anything that is, with no other outcome: a scan
//! records one problem per broken sidecar and moves on, so a panic here would
//! stop a library scan on one bad file. Input that is not UTF-8 is skipped
//! rather than lossily converted, because the production caller reads the
//! sidecar as a `String` and never sees invalid bytes.

use libfuzzer_sys::fuzz_target;
use plurx_core::scan::nfo::parse;

/// Sidecars are a few kilobytes; a megabyte covers any real one many times
/// over and keeps the XML reader's work per case bounded.
const MAX_FUZZ_BYTES: usize = 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FUZZ_BYTES {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Some(nfo) = parse(text) {
        // Every field the scan reads must be readable; `Debug` walks them all.
        let _ = format!("{nfo:?}");
    }
});
