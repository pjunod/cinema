//! Encode the K-08 M4 tokenizer corpus with whichever regex backend this
//! binary was built with, and write one line of token ids per input.
//!
//! Usage: `plurx-tokenizer-backends <model dir> <ids out>`. The model directory
//! must hold the tokenizer.json pinned in plurxd's semantic search.
//! `PLURX_TEST_TOKENIZER_CORPUS` appends a further corpus (one input per line),
//! exactly as it does for plurxd's `tokenizer_backends_agree`.
//!
//! The inputs, their order, the tokenizer settings and the output format are
//! the ones `library_search::semantic::tests::tokenizer_backends_agree` uses,
//! so the fixture part of the output must equal the ids recorded there; this
//! binary checks that before it exits, which also catches the two drifting
//! apart. Run it through `make tokenizer-backends`, which compares the two
//! builds' outputs.

use std::path::PathBuf;
use std::process::ExitCode;

use sha2::{Digest, Sha256};
use tokenizers::{Tokenizer, TruncationParams};

#[cfg(all(feature = "onig", feature = "fancy-regex"))]
compile_error!("enable exactly one backend: tokenizers uses onig whenever both are on");
#[cfg(not(any(feature = "onig", feature = "fancy-regex")))]
compile_error!("enable exactly one backend: --features onig or --features fancy-regex");

#[cfg(feature = "onig")]
const BACKEND: &str = "onig";
#[cfg(all(feature = "fancy-regex", not(feature = "onig")))]
const BACKEND: &str = "fancy-regex";

/// `FILES[1]` in crates/plurxd/src/library_search/semantic.rs.
const PINNED_TOKENIZER_SHA256: &str =
    "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";

const FIXTURE: &str =
    include_str!("../../../crates/plurxd/src/library_search/testdata/tokenizer_corpus.txt");
const RECORDED: &str =
    include_str!("../../../crates/plurxd/src/library_search/testdata/tokenizer_corpus.ids");

// The same function the plurxd test calls, from the same file.
include!("../../../crates/plurxd/src/library_search/testdata/tokenizer_programmatic_inputs.rs");

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(dir), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: plurx-tokenizer-backends <model dir> <ids out>");
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(dir);

    let raw = std::fs::read(dir.join("tokenizer.json")).expect("tokenizer.json");
    let sha = hex::encode(Sha256::digest(&raw));
    if sha != PINNED_TOKENIZER_SHA256 {
        eprintln!("not the pinned tokenizer: sha256 {sha}");
        return ExitCode::FAILURE;
    }

    // plurxd's `load_tokenizer`: no padding, truncation at 256.
    let mut tokenizer = Tokenizer::from_bytes(&raw).expect("tokenizer");
    tokenizer
        .with_padding(None)
        .with_truncation(Some(TruncationParams {
            max_length: 256,
            ..Default::default()
        }))
        .expect("truncation");

    let fixture: Vec<String> = FIXTURE
        .lines()
        .filter(|line| !line.starts_with("# "))
        .map(str::to_owned)
        .chain(programmatic_inputs())
        .collect();
    let extra = std::env::var("PLURX_TEST_TOKENIZER_CORPUS")
        .map(|path| std::fs::read_to_string(path).expect("PLURX_TEST_TOKENIZER_CORPUS"))
        .unwrap_or_default();
    let inputs: Vec<String> = fixture
        .iter()
        .cloned()
        .chain(extra.lines().map(str::to_owned))
        .collect();

    let ids: Vec<String> = inputs
        .iter()
        .map(|input| {
            tokenizer
                .encode(input.as_str(), true)
                .expect("encode")
                .get_ids()
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    std::fs::write(&out, ids.join("\n") + "\n").expect("write ids");
    println!(
        "{BACKEND}: {} inputs ({} fixture), {} ids",
        inputs.len(),
        fixture.len(),
        ids.iter()
            .map(|line| line.split(' ').count())
            .sum::<usize>()
    );

    let recorded: Vec<&str> = RECORDED.lines().collect();
    if recorded.len() != fixture.len() {
        eprintln!(
            "{BACKEND}: {} recorded lines for {} fixture inputs; the corpus and \
             tokenizer_corpus.ids have drifted apart",
            recorded.len(),
            fixture.len()
        );
        return ExitCode::FAILURE;
    }
    for (index, (actual, expected)) in ids.iter().zip(&recorded).enumerate() {
        if actual != expected {
            eprintln!(
                "{BACKEND}: first divergence from the recorded ids at fixture input \
                 {index}: {:?}",
                inputs[index]
            );
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
