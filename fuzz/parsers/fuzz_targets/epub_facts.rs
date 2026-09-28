#![no_main]

//! `read_epub_facts` over an arbitrary file: the catalogue-facts reader for
//! EPUBs, which opens a zip archive, validates entry names, reads the
//! container and package documents, and may inflate one cover image.
//!
//! A filesystem-input seam gets a tempfile harness that never reads outside
//! its own file (assessment F-build-13): the input is written to one reusable
//! temporary file and the reader is handed that path and nothing else. The
//! reader's own limits — entry count, path normalisation, the cover size cap —
//! are what keep a hostile archive bounded; this target exists to prove they
//! hold against inputs nobody wrote by hand. Any `Err` is fine; a panic, an
//! abort, or an inflate that outgrows the reduced profile is a finding.

use libfuzzer_sys::fuzz_target;
use plurx_core::metadata::book::read_epub_facts;
use std::cell::RefCell;
use std::io::{Seek, SeekFrom, Write};

/// A real EPUB's package documents fit in a few kilobytes; the cover cap is
/// the reader's own. One megabyte is enough to carry a zip bomb's central
/// directory and every truncation of a valid archive.
const MAX_FUZZ_BYTES: usize = 1024 * 1024;

// One reusable input file per thread, as in `inspect_sup`. libFuzzer ends the
// process with `exit()`, which does not run thread-local destructors, so the
// last temporary file is left behind when a campaign ends; that is accepted,
// as it is for `inspect_sup`.
thread_local! {
    static INPUT: RefCell<tempfile::NamedTempFile> = RefCell::new(
        tempfile::NamedTempFile::new().expect("create reusable fuzz input")
    );
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FUZZ_BYTES {
        return;
    }
    INPUT.with(|input| {
        let mut input = input.borrow_mut();
        let file = input.as_file_mut();
        if file.set_len(0).is_err()
            || file.seek(SeekFrom::Start(0)).is_err()
            || file.write_all(data).is_err()
            || file.flush().is_err()
        {
            return;
        }
        if let Ok(facts) = read_epub_facts(input.path()) {
            // Touch every field without formatting it: `Debug` on a cover of
            // up to the reader's 12 MiB cap would spend the budget on
            // formatting bytes rather than on the reader.
            let _ = facts.title.as_ref().map(String::len);
            let _ = facts.author.as_ref().map(String::len);
            let _ = facts.identifier.as_ref().map(String::len);
            let _ = facts.cover.as_ref().map(Vec::len);
        }
    });
});
