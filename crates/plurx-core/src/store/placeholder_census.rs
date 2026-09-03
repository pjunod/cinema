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

/// A statement literal only counts as one if it opens on a statement keyword.
const STATEMENT_KEYWORDS: [&str; 5] = ["UPDATE", "INSERT", "SELECT", "DELETE", "WITH"];

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

/// Byte ranges covered by `#[cfg(test)]` items, brace-matched over code only.
///
/// Splitting at the first `#[cfg(test)]` would be wrong: `hiqlite_media.rs`
/// declares a test module part way through the file and carries thousands of
/// production lines after it.
fn test_item_ranges(source: &str, is_code: &[bool]) -> Vec<(usize, usize)> {
    const ATTRIBUTE: &str = "#[cfg(test)]";
    let mut ranges = Vec::new();
    let mut search = 0;
    while let Some(offset) = source[search..].find(ATTRIBUTE) {
        let start = search + offset;
        search = start + ATTRIBUTE.len();
        if !is_code.get(start).copied().unwrap_or(false) {
            continue;
        }
        let mut index = search;
        let mut depth = 0_usize;
        let mut opened = false;
        let bytes = source.as_bytes();
        while index < bytes.len() {
            if !is_code[index] {
                index += 1;
                continue;
            }
            match bytes[index] {
                // A `#[cfg(test)]` that guards a `use`, a struct field or an
                // enum variant rather than a module ends before any brace.
                b';' | b',' if !opened => {
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

/// Resolve `{…}` interpolations so a template can be validated as the
/// statement it produces. Named interpolations that match a `const` in the
/// same file take that text; everything else becomes the neutral token `1`,
/// which keeps `LIMIT ${limit}` a well-formed `$N` and leaves a spliced
/// predicate clause harmless.
fn resolve_template(template: &str, constants: &[(String, String)]) -> String {
    let mut resolved = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        resolved.push_str(&rest[..open]);
        rest = &rest[open..];
        if let Some(escaped) = rest.strip_prefix("{{") {
            resolved.push_str("{{");
            rest = escaped;
            continue;
        }
        let Some(close) = rest.find('}') else {
            resolved.push_str(rest);
            return resolved;
        };
        let name = rest[1..close].split(':').next().unwrap_or_default().trim();
        match constants
            .iter()
            .find(|(constant, _)| constant == name)
            .map(|(_, value)| value)
        {
            Some(value) => resolved.push_str(value),
            None => resolved.push('1'),
        }
        rest = &rest[close + 1..];
    }
    resolved.push_str(rest);
    resolved
}

fn is_statement(text: &str) -> bool {
    if !text.contains('$') {
        return false;
    }
    let trimmed = text.trim_start();
    STATEMENT_KEYWORDS
        .iter()
        .any(|keyword| trimmed.starts_with(keyword))
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
    let mut scanned = 0_usize;
    for (name, source) in STORE_SOURCES {
        let (literals, is_code) = literals_and_code_mask(source);
        let test_ranges = test_item_ranges(source, &is_code);
        let constants = string_constants(source, &literals);
        for literal in &literals {
            if test_ranges
                .iter()
                .any(|(start, end)| literal.start >= *start && literal.start < *end)
            {
                continue;
            }
            if !is_statement(&literal.text) {
                continue;
            }
            scanned += 1;
            let statement = resolve_template(&literal.text, &constants);
            if let Err(error) = validate_sql(&statement) {
                offenders.push(format!(
                    "{name}:{}: {error}\n    {}",
                    literal.line,
                    statement.split_whitespace().collect::<Vec<_>>().join(" ")
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
}

/// The census is only repo-wide if its module list is.
#[test]
fn module_list_matches_the_directory() {
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
}

mod scanner {
    use super::{
        literals_and_code_mask, resolve_template, string_constants, test_item_ranges, validate_sql,
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
        let resolved = resolve_template(&template.text, &constants);
        assert!(resolved.contains("($2 IS NULL OR items.genre = $2)"));
        validate_sql(&resolved).expect("the assembled statement is in order");
        validate_sql(&template.text)
            .expect_err("the unassembled template is not a statement anyone runs");
    }

    /// An unresolved interpolation must leave a well-formed placeholder, or
    /// `LIMIT ${limit}` would read as a rejected non-canonical parameter.
    #[test]
    fn unresolved_interpolations_become_a_neutral_token() {
        let resolved =
            resolve_template("SELECT {projection} FROM t ORDER BY {} LIMIT ${limit}", &[]);
        assert_eq!(resolved, "SELECT 1 FROM t ORDER BY 1 LIMIT $1");
        validate_sql(&resolved).expect("the neutral token keeps the statement well formed");
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
