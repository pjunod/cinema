//! A repo-wide census of the placeholder rule every replicated statement is
//! held to at runtime.
//!
//! hiqlite binds parameters by rusqlite's numeric index while SQLite treats
//! `$N` as a *named* parameter indexed by first appearance. A statement that
//! introduces `$6` before `$4` is therefore refused by
//! [`validate_sql`](super::hiqlite::validate_sql) *before any I/O* — and every
//! caller in this tree discards or misreads that error, so the refusal is
//! silent. `ce253a55` shipped two such statements (the fragment-index lease
//! renew and yield) and the queue built nothing for three days;
//! `c59e4a45` shipped four more in `dv_conversions`.
//!
//! This defect class already has four ledger rows. The first fix added the
//! same census to `hiqlite_sessions.rs` alone, and six weeks later the mistake
//! landed in two other modules — so the census lives here, over every
//! replicated store slice, and the module list is checked against the
//! directory rather than trusted.
//!
//! There is deliberately no per-statement exemption marker: a marker is a
//! loophole a future production statement can opt into. A statement assembled
//! by `format!` is validated with its interpolations resolved (from the file's
//! own `const … : &str` where one exists, and as the neutral token `1`
//! otherwise), which is why a fragment such as `hiqlite_media`'s `GENRE` — a
//! predicate that legitimately opens on `$2` — is judged as part of the
//! statement it is spliced into rather than on its own.

use super::hiqlite::validate_sql;

/// Every replicated store slice, by file name and source text.
///
/// [`the module list matches the directory`](module_list_matches_the_directory)
/// fails if a new `hiqlite*.rs` appears without being added here.
const STORE_SOURCES: &[(&str, &str)] = &[
    ("hiqlite.rs", include_str!("hiqlite.rs")),
    ("hiqlite_catalog.rs", include_str!("hiqlite_catalog.rs")),
    (
        "hiqlite_coordination.rs",
        include_str!("hiqlite_coordination.rs"),
    ),
    ("hiqlite_durable.rs", include_str!("hiqlite_durable.rs")),
    (
        "hiqlite_dv_conversion.rs",
        include_str!("hiqlite_dv_conversion.rs"),
    ),
    (
        "hiqlite_fragment_index_cluster.rs",
        include_str!("hiqlite_fragment_index_cluster.rs"),
    ),
    ("hiqlite_import.rs", include_str!("hiqlite_import.rs")),
    ("hiqlite_media.rs", include_str!("hiqlite_media.rs")),
    (
        "hiqlite_pretranscode.rs",
        include_str!("hiqlite_pretranscode.rs"),
    ),
    (
        "hiqlite_publication.rs",
        include_str!("hiqlite_publication.rs"),
    ),
    ("hiqlite_reading.rs", include_str!("hiqlite_reading.rs")),
    ("hiqlite_sessions.rs", include_str!("hiqlite_sessions.rs")),
    (
        "hiqlite_shared_cache.rs",
        include_str!("hiqlite_shared_cache.rs"),
    ),
    (
        "hiqlite_timeline_annotations.rs",
        include_str!("hiqlite_timeline_annotations.rs"),
    ),
];

/// The shared store modules a replicated slice splices `const` SQL from.
///
/// `hiqlite_fragment_index_cluster.rs` builds its history statements around
/// `ANALYSIS_CANONICAL_CTE`, which lives in `fragment_index_cluster.rs`. Without
/// these, such a statement resolves to a neutral token, stops looking like a
/// statement, and is never judged.
const SHARED_CONSTANT_SOURCES: &[(&str, &str)] = &[
    ("dv_conversion.rs", include_str!("dv_conversion.rs")),
    ("fragindex.rs", include_str!("fragindex.rs")),
    (
        "fragment_index_cluster.rs",
        include_str!("fragment_index_cluster.rs"),
    ),
    ("mod.rs", include_str!("mod.rs")),
    ("publication.rs", include_str!("publication.rs")),
    ("renditionplan.rs", include_str!("renditionplan.rs")),
    ("telemetry.rs", include_str!("telemetry.rs")),
    (
        "timeline_annotations.rs",
        include_str!("timeline_annotations.rs"),
    ),
];

/// A statement literal only counts as one if it opens on a statement keyword —
/// judged *after* its interpolations are resolved, because a template can open
/// on one.
const STATEMENT_KEYWORDS: [&str; 5] = ["UPDATE", "INSERT", "SELECT", "DELETE", "WITH"];

/// How many `$`-bearing SQL-shaped literals the census reads as fragments
/// rather than statements: predicate clauses that are only ever spliced into a
/// host, and whose host is judged instead.
///
/// This is a census, so the number is asserted. A new fragment is not
/// forbidden — it has to be looked at, and this number updated, which is what
/// stops a whole statement from disappearing behind an interpolation.
///
/// The four today, each spliced into a host this census does judge:
/// `hiqlite_media.rs`'s two `GENRE` predicates (into the item count and the
/// item page), and `hiqlite_durable.rs`'s two membership tombstone arms (into
/// the offline-package insert, once per arm).
const EXPECTED_FRAGMENTS: usize = 4;

/// One Rust string literal, with its escapes decoded.
struct Literal {
    line: usize,
    start: usize,
    text: String,
}

/// Decode every string literal in a Rust source file, and mark which bytes
/// are code (so brace matching can ignore braces inside strings and comments).
fn literals_and_code_mask(source: &str) -> (Vec<Literal>, Vec<bool>) {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut is_code = vec![true; bytes.len()];
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                let end = source[index..]
                    .find('\n')
                    .map_or(bytes.len(), |offset| index + offset);
                blank(&mut is_code, index, end);
                index = end;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let mut depth = 1_usize;
                let mut cursor = index + 2;
                while cursor < bytes.len() && depth > 0 {
                    if source[cursor..].starts_with("/*") {
                        depth += 1;
                        cursor += 2;
                    } else if source[cursor..].starts_with("*/") {
                        depth -= 1;
                        cursor += 2;
                    } else {
                        cursor += 1;
                    }
                }
                blank(&mut is_code, index, cursor);
                index = cursor;
            }
            b'r' if matches!(bytes.get(index + 1), Some(b'#' | b'"')) => {
                let mut cursor = index + 1;
                let mut hashes = 0_usize;
                while bytes.get(cursor) == Some(&b'#') {
                    hashes += 1;
                    cursor += 1;
                }
                if bytes.get(cursor) != Some(&b'"') {
                    index += 1;
                    continue;
                }
                let content = cursor + 1;
                let terminator = format!("\"{}", "#".repeat(hashes));
                let end = source[content..]
                    .find(&terminator)
                    .map_or(bytes.len(), |offset| content + offset);
                literals.push(Literal {
                    line: line_of(source, index),
                    start: index,
                    text: source[content..end.min(bytes.len())].to_owned(),
                });
                let after = (end + terminator.len()).min(bytes.len());
                blank(&mut is_code, index, after);
                index = after;
            }
            b'"' => {
                let (text, end) = decode_string(source, index);
                literals.push(Literal {
                    line: line_of(source, index),
                    start: index,
                    text,
                });
                blank(&mut is_code, index, end);
                index = end;
            }
            b'\'' => {
                // A char literal, or a lifetime. Only the former can hide a brace.
                let rest = &source[index..];
                let char_literal = rest
                    .strip_prefix('\'')
                    .and_then(|body| {
                        let mut chars = body.chars();
                        let first = chars.next()?;
                        let width = if first == '\\' {
                            1 + chars.next().map_or(0, char::len_utf8)
                        } else {
                            first.len_utf8()
                        };
                        (body.as_bytes().get(width) == Some(&b'\'')).then_some(width + 2)
                    })
                    .unwrap_or(0);
                if char_literal > 0 {
                    blank(&mut is_code, index, index + char_literal);
                    index += char_literal;
                } else {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    (literals, is_code)
}

fn blank(is_code: &mut [bool], start: usize, end: usize) {
    let end = end.min(is_code.len());
    for flag in is_code.iter_mut().take(end).skip(start) {
        *flag = false;
    }
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count() + 1
}

/// Decode one `"…"` literal, returning its text and the offset just past it.
fn decode_string(source: &str, start: usize) -> (String, usize) {
    let bytes = source.as_bytes();
    let mut text = String::new();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                let escape = bytes.get(index + 1).copied().unwrap_or(b'\\');
                index += 2;
                match escape {
                    b'n' => text.push('\n'),
                    b't' => text.push('\t'),
                    b'r' => text.push('\r'),
                    b'0' => text.push('\0'),
                    b'\n' => {
                        // A trailing backslash continues the literal on the
                        // next line and swallows that line's indentation.
                        while matches!(bytes.get(index), Some(b' ' | b'\t')) {
                            index += 1;
                        }
                    }
                    b'x' => {
                        let end = (index + 2).min(bytes.len());
                        if let Ok(code) = u8::from_str_radix(&source[index..end], 16) {
                            text.push(char::from(code));
                        }
                        index = end;
                    }
                    b'u' => {
                        if let Some(close) = source[index..].find('}') {
                            let digits = source[index..index + close].trim_start_matches('{');
                            if let Some(character) = u32::from_str_radix(digits, 16)
                                .ok()
                                .and_then(char::from_u32)
                            {
                                text.push(character);
                            }
                            index += close + 1;
                        }
                    }
                    other => text.push(char::from(other)),
                }
            }
            b'"' => return (text, index + 1),
            byte => {
                // Multi-byte characters arrive one byte at a time; push the
                // whole character and skip its continuation bytes.
                let width = utf8_width(byte);
                let end = (index + width).min(bytes.len());
                text.push_str(&source[index..end]);
                index = end;
            }
        }
    }
    (text, bytes.len())
}

fn utf8_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Byte ranges covered by test-only items, brace-matched over code only.
///
/// Splitting at the first attribute would be wrong: `hiqlite_media.rs`
/// declares a test module part way through the file and carries thousands of
/// production lines after it.
///
/// Any `#[cfg(…)]` whose predicate mentions `test` counts, not only the exact
/// `#[cfg(test)]` spelling — `#[cfg(all(test, feature = "hiqlite-store"))]`
/// guards a test module just as completely, and this tree already gates
/// test-adjacent items on compound predicates.
fn test_item_ranges(source: &str, is_code: &[bool]) -> Vec<(usize, usize)> {
    const ATTRIBUTE: &str = "#[cfg(";
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut search = 0;
    while let Some(offset) = source[search..].find(ATTRIBUTE) {
        let start = search + offset;
        search = start + ATTRIBUTE.len();
        if !is_code.get(start).copied().unwrap_or(false) {
            continue;
        }
        // The predicate, up to the attribute's closing bracket.
        let Some(predicate_end) = source[start..].find(']').map(|end| start + end) else {
            continue;
        };
        let predicate = &source[start + ATTRIBUTE.len()..predicate_end];
        if predicate.contains("not(test") {
            continue;
        }
        if !predicate
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|token| token == "test")
        {
            continue;
        }
        let mut index = predicate_end + 1;
        let mut depth = 0_usize;
        let mut parens = 0_usize;
        let mut opened = false;
        while index < bytes.len() {
            if !is_code[index] {
                index += 1;
                continue;
            }
            match bytes[index] {
                b'(' | b'[' => parens += 1,
                b')' | b']' => parens = parens.saturating_sub(1),
                // A test-only `use`, struct field or enum variant ends before
                // any brace — but a `;` or `,` inside a parameter list or an
                // attribute does not end anything.
                b';' | b',' if !opened && parens == 0 => {
                    index += 1;
                    break;
                }
                b'}' if !opened => break,
                b'{' => {
                    depth += 1;
                    opened = true;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        index += 1;
                        break;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        ranges.push((start, index));
    }
    ranges
}

/// `const NAME: &str = "…"` definitions, so a `format!` template can be
/// judged as the statement it becomes.
fn string_constants(source: &str, literals: &[Literal]) -> Vec<(String, String)> {
    let mut constants = Vec::new();
    let mut search = 0;
    while let Some(offset) = source[search..].find("const ") {
        let start = search + offset;
        search = start + "const ".len();
        let Some(colon) = source[search..].find(':') else {
            break;
        };
        let name = source[search..search + colon].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            continue;
        }
        let tail = &source[search + colon..];
        let Some(equals) = tail.find('=') else {
            continue;
        };
        if !tail[..equals].contains("str") {
            continue;
        }
        let value_at = search + colon + equals;
        if let Some(literal) = literals.iter().find(|literal| literal.start > value_at) {
            if source[value_at..literal.start]
                .trim()
                .trim_start_matches('=')
                .trim()
                .is_empty()
            {
                constants.push((name.to_owned(), literal.text.clone()));
            }
        }
    }
    constants
}

/// `let name = "…"` bindings, with every arm a conditional can choose.
///
/// `hiqlite_durable.rs` splices a membership tombstone clause chosen at
/// runtime, and two of its three arms carry their own `$N`. Resolving the name
/// to a neutral token would leave both real statements unjudged, so each arm
/// is validated as its own statement.
fn string_bindings(
    source: &str,
    literals: &[Literal],
    is_code: &[bool],
) -> Vec<(String, Vec<String>)> {
    let bytes = source.as_bytes();
    let mut bindings: Vec<(String, Vec<String>)> = Vec::new();
    let mut search = 0;
    while let Some(offset) = source[search..].find("let ") {
        let start = search + offset;
        search = start + "let ".len();
        if !is_code.get(start).copied().unwrap_or(false) {
            continue;
        }
        let tail = &source[search..];
        let name_end = tail
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(tail.len());
        let name = &tail[..name_end];
        if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_lowercase()) {
            continue;
        }
        // Walk to the `;` that closes the binding, ignoring braces it opens.
        let mut index = search + name_end;
        let mut depth = 0_usize;
        while index < bytes.len() {
            if is_code[index] {
                match bytes[index] {
                    b'{' => depth += 1,
                    b'}' => depth = depth.saturating_sub(1),
                    b';' if depth == 0 => break,
                    _ => {}
                }
            }
            index += 1;
        }
        // Only SQL-shaped arms count. A binding's span also swallows literals
        // belonging to nested calls — `hiqlite_import.rs` builds its cursor
        // filter from a `format!("${index}")` inside the same `let` — and
        // splicing one of those in produces a statement nobody writes.
        let arms = literals
            .iter()
            .filter(|literal| literal.start > start && literal.start < index)
            .filter(|literal| literal.text.trim().is_empty() || is_sql_shaped(&literal.text))
            .map(|literal| literal.text.clone())
            .take(MAX_BINDING_ARMS)
            .collect::<Vec<_>>();
        if !arms.is_empty() {
            bindings.push((name.to_owned(), arms));
        }
    }
    bindings
}

/// At most this many arms per binding, and this many assembled variants per
/// template. A runtime-chosen fragment with more shapes than this is a
/// statement that should be written out, not a combinatorial explosion.
const MAX_BINDING_ARMS: usize = 4;
const MAX_TEMPLATE_VARIANTS: usize = 16;

/// Every statement a `format!` template can produce.
///
/// A named interpolation resolves to a `const` in the same file, then to one
/// in a shared store module, then to each arm of a same-file `let` binding.
/// Anything still unresolved becomes the neutral token `1`, which keeps
/// `LIMIT ${limit}` a well-formed `$N` and leaves a spliced identifier inert.
fn resolve_template(
    template: &str,
    constants: &[(String, String)],
    bindings: &[(String, Vec<String>)],
) -> Vec<String> {
    let mut variants = vec![String::with_capacity(template.len())];
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        for variant in &mut variants {
            variant.push_str(&rest[..open]);
        }
        rest = &rest[open..];
        if let Some(escaped) = rest.strip_prefix("{{") {
            for variant in &mut variants {
                variant.push_str("{{");
            }
            rest = escaped;
            continue;
        }
        let Some(close) = rest.find('}') else {
            for variant in &mut variants {
                variant.push_str(rest);
            }
            return variants;
        };
        let name = rest[1..close].split(':').next().unwrap_or_default().trim();
        let choices = constants
            .iter()
            .find(|(constant, _)| constant == name)
            .map(|(_, value)| vec![value.clone()])
            .or_else(|| {
                bindings
                    .iter()
                    .find(|(binding, _)| binding == name)
                    .map(|(_, arms)| arms.clone())
            })
            .unwrap_or_else(|| vec!["1".to_owned()]);
        variants = variants
            .iter()
            .flat_map(|variant| {
                choices.iter().map(move |choice| {
                    let mut next = variant.clone();
                    next.push_str(choice);
                    next
                })
            })
            .take(MAX_TEMPLATE_VARIANTS)
            .collect();
        rest = &rest[close + 1..];
    }
    for variant in &mut variants {
        variant.push_str(rest);
    }
    variants
}

/// Does the text carry a statement keyword as a whole word anywhere in it?
fn is_sql_shaped(text: &str) -> bool {
    STATEMENT_KEYWORDS.iter().any(|keyword| {
        text.match_indices(keyword).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + keyword.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

/// A literal worth judging at all: SQL-shaped, and parameterised.
fn is_sql_candidate(text: &str) -> bool {
    text.contains('$') && is_sql_shaped(text)
}

/// A resolved candidate is a statement when it opens on a statement keyword.
/// Anything else is a fragment of one, and its host is judged instead.
fn is_statement(text: &str) -> bool {
    let trimmed = text.trim_start();
    STATEMENT_KEYWORDS
        .iter()
        .any(|keyword| trimmed.starts_with(keyword))
}

fn constants_for(name: &str, source: &str, literals: &[Literal]) -> Vec<(String, String)> {
    // Same-file definitions win: two slices may both define `ITEM_COLS`.
    let mut constants = string_constants(source, literals);
    for (shared_name, shared) in SHARED_CONSTANT_SOURCES {
        if *shared_name == name {
            continue;
        }
        let (shared_literals, _) = literals_and_code_mask(shared);
        for (constant, value) in string_constants(shared, &shared_literals) {
            if !constants.iter().any(|(known, _)| *known == constant) {
                constants.push((constant, value));
            }
        }
    }
    constants
}

/// Every replicated statement in the store must pass the validator the store
/// applies at runtime.
///
/// A statement that fails here is refused in production before any I/O, and
/// the refusal is invisible at the call site — see
/// `docs/FRAGMENT-INDEX-QUEUE-REPAIR-HANDOFF.md` §1.
#[test]
fn every_replicated_placeholder_is_introduced_in_order() {
    let mut offenders = Vec::new();
    let mut fragments = Vec::new();
    let mut scanned = 0_usize;
    for (name, source) in STORE_SOURCES {
        let (literals, is_code) = literals_and_code_mask(source);
        let test_ranges = test_item_ranges(source, &is_code);
        let constants = constants_for(name, source, &literals);
        let bindings = string_bindings(source, &literals, &is_code);
        for literal in &literals {
            if test_ranges
                .iter()
                .any(|(start, end)| literal.start >= *start && literal.start < *end)
            {
                continue;
            }
            if !is_sql_candidate(&literal.text) {
                continue;
            }
            let mut judged = false;
            for statement in resolve_template(&literal.text, &constants, &bindings) {
                if !is_statement(&statement) {
                    continue;
                }
                judged = true;
                scanned += 1;
                if let Err(error) = validate_sql(&statement) {
                    offenders.push(format!(
                        "{name}:{}: {error}\n    {}",
                        literal.line,
                        statement.split_whitespace().collect::<Vec<_>>().join(" ")
                    ));
                }
            }
            if !judged {
                fragments.push(format!(
                    "{name}:{}: {}",
                    literal.line,
                    literal
                        .text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
        }
    }
    assert!(
        scanned > 400,
        "the census found only {scanned} statements; the scanner is broken, not the tree"
    );
    assert!(
        offenders.is_empty(),
        "replicated statements bind by first appearance, so these are refused before any I/O:\n{}",
        offenders.join("\n")
    );
    assert_eq!(
        fragments.len(),
        EXPECTED_FRAGMENTS,
        "a parameterised SQL literal that resolves to no statement is judged by nothing. \
         Look at each of these, then update EXPECTED_FRAGMENTS:\n{}",
        fragments.join("\n")
    );
}

/// The census is only repo-wide if its module list is, and only honest if each
/// name is paired with its own file.
///
/// The name list alone is not enough: `("hiqlite_reading.rs",
/// include_str!("hiqlite_sessions.rs"))` would still match the directory while
/// leaving one slice uncensused and censusing another twice.
#[test]
fn the_census_covers_every_slice_exactly_once() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store");
    let mut present = std::fs::read_dir(&directory)
        .expect("the store module directory is readable from the source tree")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("hiqlite") && name.ends_with(".rs"))
        .collect::<Vec<_>>();
    present.sort();
    let mut censused = STORE_SOURCES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<Vec<_>>();
    censused.sort();
    assert_eq!(
        present, censused,
        "every replicated store slice must be in STORE_SOURCES; a new one is not exempt"
    );
    for (name, source) in STORE_SOURCES.iter().chain(SHARED_CONSTANT_SOURCES) {
        let on_disk = std::fs::read_to_string(directory.join(name))
            .unwrap_or_else(|error| panic!("{name} is readable: {error}"));
        assert_eq!(
            on_disk.len(),
            source.len(),
            "{name} is paired with another file's source"
        );
    }
}

mod scanner {
    use super::{
        is_sql_candidate, is_statement, literals_and_code_mask, resolve_template, string_bindings,
        string_constants, test_item_ranges, validate_sql,
    };

    /// A statement inside a `#[cfg(test)]` module is a fixture, not a
    /// production statement, and several deliberately violate the rule.
    #[test]
    fn test_modules_are_stripped_by_brace_matching() {
        let source = r#"
fn production() { let _ = "SELECT $1 FROM a"; }
#[cfg(test)]
mod tests {
    fn fixture() { let _ = "SELECT $2, $1 FROM a"; }
}
fn later_production() { let _ = "SELECT $1, $2 FROM b"; }
"#;
        let (literals, is_code) = literals_and_code_mask(source);
        let ranges = test_item_ranges(source, &is_code);
        let visible = literals
            .iter()
            .filter(|literal| {
                !ranges
                    .iter()
                    .any(|(start, end)| literal.start >= *start && literal.start < *end)
            })
            .map(|literal| literal.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(visible, vec!["SELECT $1 FROM a", "SELECT $1, $2 FROM b"]);
    }

    /// A `format!` template is judged as the statement it becomes: a spliced
    /// predicate that opens on `$2` is correct in place.
    #[test]
    fn templates_are_resolved_before_validation() {
        let source = r#"
const GENRE: &str = "($2 IS NULL OR items.genre = $2)";
fn page() { let _ = format!("SELECT id FROM items WHERE library_id = $1 AND {GENRE} LIMIT $3"); }
"#;
        let (literals, _) = literals_and_code_mask(source);
        let constants = string_constants(source, &literals);
        let template = literals
            .iter()
            .find(|literal| literal.text.starts_with("SELECT"))
            .expect("the template literal is scanned");
        let resolved = resolve_template(&template.text, &constants, &[]);
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].contains("($2 IS NULL OR items.genre = $2)"));
        validate_sql(&resolved[0]).expect("the assembled statement is in order");
        validate_sql(&template.text)
            .expect_err("the unassembled template is not a statement anyone runs");
    }

    /// A statement that *opens* on an interpolation must still be judged. This
    /// is how `analysis_history` — eight placeholders, in the very file this
    /// census was written for — escaped the first version of the scan.
    #[test]
    fn a_statement_that_opens_on_an_interpolation_is_still_judged() {
        let source = r#"
const CTE: &str = "WITH matching AS (SELECT id FROM jobs WHERE cache_key = $1)";
fn history() { let _ = format!("{CTE} SELECT id FROM matching WHERE fence > $3 AND id > $2"); }
"#;
        let (literals, _) = literals_and_code_mask(source);
        let constants = string_constants(source, &literals);
        let template = literals
            .iter()
            .find(|literal| literal.text.starts_with("{CTE}"))
            .expect("the template literal is scanned");
        assert!(is_sql_candidate(&template.text));
        let resolved = resolve_template(&template.text, &constants, &[]);
        assert!(
            is_statement(&resolved[0]),
            "resolution has to happen before the statement test, not after"
        );
        validate_sql(&resolved[0])
            .expect_err("the assembled statement introduces $3 before $2 and must be caught");
    }

    /// A fragment chosen at runtime is judged once per arm, so a conditional
    /// clause carrying its own placeholders cannot hide behind a neutral token.
    #[test]
    fn every_arm_of_a_runtime_fragment_is_judged() {
        let source = r#"
fn insert() {
    let clause = if wide {
        "AND EXISTS (SELECT 1 FROM removals WHERE node_id = $5)"
    } else {
        "AND EXISTS (SELECT 1 FROM removals WHERE node_id = $9)"
    };
    let sql = format!("INSERT INTO packages SELECT $1, $2, $3, $4, $5, $6 WHERE TRUE {clause}");
}
"#;
        let (literals, is_code) = literals_and_code_mask(source);
        let bindings = string_bindings(source, &literals, &is_code);
        assert_eq!(
            bindings.len(),
            2,
            "one binding per `let` that names literals"
        );
        let template = literals
            .iter()
            .find(|literal| literal.text.starts_with("INSERT"))
            .expect("the template literal is scanned");
        let variants = resolve_template(&template.text, &[], &bindings);
        assert_eq!(variants.len(), 2, "one statement per arm");
        validate_sql(&variants[0]).expect("the in-order arm is fine");
        validate_sql(&variants[1]).expect_err("the arm that skips to $9 must be caught");
    }

    /// An unresolved interpolation must leave a well-formed placeholder, or
    /// `LIMIT ${limit}` would read as a rejected non-canonical parameter.
    #[test]
    fn unresolved_interpolations_become_a_neutral_token() {
        let resolved = resolve_template(
            "SELECT {projection} FROM t ORDER BY {} LIMIT ${limit}",
            &[],
            &[],
        );
        assert_eq!(
            resolved,
            vec!["SELECT 1 FROM t ORDER BY 1 LIMIT $1".to_owned()]
        );
        validate_sql(&resolved[0]).expect("the neutral token keeps the statement well formed");
    }

    /// A compound `cfg` guards a test module just as completely as the bare
    /// spelling, and a test fixture is allowed to violate the rule.
    #[test]
    fn a_compound_test_cfg_is_stripped_too() {
        let source = r#"
fn production() { let _ = "SELECT $1 FROM a"; }
#[cfg(all(test, feature = "hiqlite-store"))]
mod tests {
    fn fixture(first: i64, second: i64) -> &'static str { "SELECT $2, $1 FROM a" }
}
#[cfg(not(test))]
fn shipped() { let _ = "SELECT $1, $2 FROM b"; }
"#;
        let (literals, is_code) = literals_and_code_mask(source);
        let ranges = test_item_ranges(source, &is_code);
        let visible = literals
            .iter()
            .filter(|literal| {
                !ranges
                    .iter()
                    .any(|(start, end)| literal.start >= *start && literal.start < *end)
            })
            .map(|literal| literal.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(visible, vec!["SELECT $1 FROM a", "SELECT $1, $2 FROM b"]);
    }

    /// A `#[cfg(test)]` function with more than one parameter must not end at
    /// the comma in its signature, or its body escapes the strip.
    #[test]
    fn a_test_function_signature_does_not_end_the_item() {
        let source = r#"
#[cfg(test)]
fn fixture(first: i64, second: i64) -> &'static str { "SELECT $2, $1 FROM a" }
fn production() { let _ = "SELECT $1 FROM b"; }
"#;
        let (literals, is_code) = literals_and_code_mask(source);
        let ranges = test_item_ranges(source, &is_code);
        let visible = literals
            .iter()
            .filter(|literal| {
                !ranges
                    .iter()
                    .any(|(start, end)| literal.start >= *start && literal.start < *end)
            })
            .map(|literal| literal.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(visible, vec!["SELECT $1 FROM b"]);
    }

    /// The mutation this census exists to catch: swapping the first
    /// appearances of two placeholders.
    #[test]
    fn swapping_a_first_appearance_is_rejected() {
        let error = validate_sql(
            "UPDATE jobs SET lease_expires_ms = $1, updated_at_ms = $2 \
             WHERE cache_key = $3 AND target_node_id = $6 AND owner_node_id = $4 AND fence = $5",
        )
        .expect_err("an out-of-order first appearance is refused");
        assert!(
            error.to_string().contains("expected $4, found $6"),
            "the error names the ordinal it expected: {error}"
        );
    }
}
