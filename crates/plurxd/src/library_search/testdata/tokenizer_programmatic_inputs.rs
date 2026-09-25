// K-08 M4's tokenizer corpus, the part a text fixture cannot carry. Included
// with `include!` by `library_search::semantic::tests` and by the two-backend
// harness in spikes/tokenizer-backends, so both encode the same inputs in the
// same order after `tokenizer_corpus.txt`. The ids for all of them are
// recorded in `tokenizer_corpus.ids`.

/// Inputs a text fixture cannot carry: control characters the normalizer
/// strips, the empty and whitespace-only strings, and one input past the
/// 256-token truncation.
fn programmatic_inputs() -> Vec<String> {
    vec![
        String::new(),
        " ".to_owned(),
        "\t\r\n".to_owned(),
        "bell\u{7}and\u{0}nul and\u{1b}[31mescape".to_owned(),
        "line one\nline two\r\nline three".to_owned(),
        "\u{fffd} replacement and \u{e000} private use".to_owned(),
        "word ".repeat(400),
    ]
}
