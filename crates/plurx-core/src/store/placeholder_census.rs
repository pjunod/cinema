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
pub(super) const STORE_SOURCES: &[(&str, &str)] = &[
    ("hiqlite.rs", include_str!("hiqlite.rs")),
    (
        "hiqlite_annotations.rs",
        include_str!("hiqlite_annotations.rs"),
    ),
    (
        "hiqlite_background_jobs.rs",
        include_str!("hiqlite_background_jobs.rs"),
    ),
    (
        "hiqlite_jellyfin_catalog.rs",
        include_str!("hiqlite_jellyfin_catalog.rs"),
    ),
    ("hiqlite_catalog.rs", include_str!("hiqlite_catalog.rs")),
    (
        "hiqlite_classification.rs",
        include_str!("hiqlite_classification.rs"),
    ),
    (
        "hiqlite_coordination.rs",
        include_str!("hiqlite_coordination.rs"),
    ),
    ("hiqlite_durable.rs", include_str!("hiqlite_durable.rs")),
    (
        "hiqlite_dv_conversion.rs",
        include_str!("hiqlite_dv_conversion.rs"),
    ),
    ("hiqlite_dvr.rs", include_str!("hiqlite_dvr.rs")),
    (
        "hiqlite_fragment_index_cluster.rs",
        include_str!("hiqlite_fragment_index_cluster.rs"),
    ),
    ("hiqlite_import.rs", include_str!("hiqlite_import.rs")),
    ("hiqlite_remote.rs", include_str!("hiqlite_remote.rs")),
    (
        "hiqlite_invitations.rs",
        include_str!("hiqlite_invitations.rs"),
    ),
    (
        "hiqlite_jellyfin_play.rs",
        include_str!("hiqlite_jellyfin_play.rs"),
    ),
    (
        "hiqlite_jellyfin_login.rs",
        include_str!("hiqlite_jellyfin_login.rs"),
    ),
    (
        "hiqlite_playback_handoffs.rs",
        include_str!("hiqlite_playback_handoffs.rs"),
    ),
    (
        "hiqlite_jellyfin_identity.rs",
        include_str!("hiqlite_jellyfin_identity.rs"),
    ),
    (
        "hiqlite_library_channels.rs",
        include_str!("hiqlite_library_channels.rs"),
    ),
    (
        "hiqlite_live_tv_resource.rs",
        include_str!("hiqlite_live_tv_resource.rs"),
    ),
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
    ("hiqlite_sharing.rs", include_str!("hiqlite_sharing.rs")),
    (
        "hiqlite_shared_cache.rs",
        include_str!("hiqlite_shared_cache.rs"),
    ),
    (
        "hiqlite_timeline_annotations.rs",
        include_str!("hiqlite_timeline_annotations.rs"),
    ),
];

/// Every standalone SQLite store slice, by file name and source text.
///
/// SQLite's `?N` indices are numeric rather than first-appearance names, so
/// this list feeds a separate validator below. The directory equality test
/// makes adding a module without adding its SQL to the census impossible.
const SQLITE_SOURCES: &[(&str, &str)] = &[
    ("annotations.rs", include_str!("sqlite/annotations.rs")),
    ("apikeys.rs", include_str!("sqlite/apikeys.rs")),
    (
        "background_jobs.rs",
        include_str!("sqlite/background_jobs.rs"),
    ),
    ("cache.rs", include_str!("sqlite/cache.rs")),
    (
        "classification.rs",
        include_str!("sqlite/classification.rs"),
    ),
    ("coordination.rs", include_str!("sqlite/coordination.rs")),
    ("dv_conversion.rs", include_str!("sqlite/dv_conversion.rs")),
    ("file_grants.rs", include_str!("sqlite/file_grants.rs")),
    ("dvr.rs", include_str!("sqlite/dvr.rs")),
    ("fragindex.rs", include_str!("sqlite/fragindex.rs")),
    (
        "fragment_index_cluster.rs",
        include_str!("sqlite/fragment_index_cluster.rs"),
    ),
    ("housekeeping.rs", include_str!("sqlite/housekeeping.rs")),
    ("remote.rs", include_str!("sqlite/remote.rs")),
    ("invitations.rs", include_str!("sqlite/invitations.rs")),
    (
        "jellyfin_identity.rs",
        include_str!("sqlite/jellyfin_identity.rs"),
    ),
    (
        "jellyfin_catalog.rs",
        include_str!("sqlite/jellyfin_catalog.rs"),
    ),
    ("jellyfin_play.rs", include_str!("sqlite/jellyfin_play.rs")),
    (
        "jellyfin_login.rs",
        include_str!("sqlite/jellyfin_login.rs"),
    ),
    ("library.rs", include_str!("sqlite/library.rs")),
    (
        "library_channels.rs",
        include_str!("sqlite/library_channels.rs"),
    ),
    (
        "live_tv_resource.rs",
        include_str!("sqlite/live_tv_resource.rs"),
    ),
    ("media.rs", include_str!("sqlite/media.rs")),
    ("mod.rs", include_str!("sqlite/mod.rs")),
    ("offline.rs", include_str!("sqlite/offline.rs")),
    ("outbox.rs", include_str!("sqlite/outbox.rs")),
    (
        "playback_handoffs.rs",
        include_str!("sqlite/playback_handoffs.rs"),
    ),
    ("pretranscode.rs", include_str!("sqlite/pretranscode.rs")),
    ("publication.rs", include_str!("sqlite/publication.rs")),
    ("reading.rs", include_str!("sqlite/reading.rs")),
    ("sessions.rs", include_str!("sqlite/sessions.rs")),
    ("shared_cache.rs", include_str!("sqlite/shared_cache.rs")),
    ("sharing.rs", include_str!("sqlite/sharing.rs")),
    ("telemetry.rs", include_str!("sqlite/telemetry.rs")),
    (
        "timeline_annotations.rs",
        include_str!("sqlite/timeline_annotations.rs"),
    ),
    ("trakt.rs", include_str!("sqlite/trakt.rs")),
    ("users.rs", include_str!("sqlite/users.rs")),
    ("watch.rs", include_str!("sqlite/watch.rs")),
];

/// The shared store modules a replicated slice splices `const` SQL from.
///
/// `hiqlite_fragment_index_cluster.rs` builds its history statements around
/// `ANALYSIS_CANONICAL_CTE`, which lives in `fragment_index_cluster.rs`. Without
/// these, such a statement resolves to a neutral token, stops looking like a
/// statement, and is never judged.
const SHARED_CONSTANT_SOURCES: &[(&str, &str)] = &[
    ("playback_handoffs.rs", include_str!("playback_handoffs.rs")),
    ("jellyfin_login.rs", include_str!("jellyfin_login.rs")),
    ("jellyfin_catalog.rs", include_str!("jellyfin_catalog.rs")),
    ("jellyfin_play.rs", include_str!("jellyfin_play.rs")),
    ("jellyfin_watch.rs", include_str!("jellyfin_watch.rs")),
    (
        "../live_tv_resource.rs",
        include_str!("../live_tv_resource.rs"),
    ),
    (
        "downloaded_subtitles.rs",
        include_str!("downloaded_subtitles.rs"),
    ),
    ("continuous_family.rs", include_str!("continuous_family.rs")),
    (
        "quality_cancellation.rs",
        include_str!("quality_cancellation.rs"),
    ),
    ("quality_ledger.rs", include_str!("quality_ledger.rs")),
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
/// The ten today, each spliced into a host this census or its composition regression judges:
/// `hiqlite_media.rs`'s two `GENRE` predicates (into the item count and the
/// item page), `hiqlite_durable.rs`'s two membership tombstone arms (into the
/// offline-package insert, once per arm), and `hiqlite.rs`'s
/// `CANONICAL_SETTINGS_GENERATION_PREDICATE`, `hiqlite_library_channels.rs`'s
/// completed-request guard, and `hiqlite_publication.rs`'s artwork-repair
/// exclusion, plus `hiqlite_sessions.rs`'s `LocalSessionSql::existing_user`
/// predicate, expanded through the actual finite helper at each host call.
/// The remaining two are `hiqlite_media.rs`'s reconcile GUARD and its generated
/// job-lease fence. The guard is spliced into each removal template; the fence
/// starts at `bound.len() + 1`, appends all five lease bindings and is substituted
/// before the complete statement is passed to `validate_sql`.
///
/// The canonical settings predicate opens on `$4` because it is the tail of the statement it is
/// spliced into: `put_settings_if_generation` selects `$1, $2, $3` and then
/// guards the write on the caller's expected generation. Judged on its own it
/// would look like a statement introducing `$4` before `$1`; judged as part of
/// its host -- which is how the census reads it, and the whole reason
/// interpolations are resolved -- the introduction order is `$1` through `$5`
/// and the statement is accepted. It is also evaluated directly, as a bare
/// `SELECT {…}`, by `replicated_generation_guard_matches_only_canonical_integer_state`;
/// that is a `#[cfg(test)]` fixture bound by rusqlite's positional `params!`,
/// stripped before this census runs, and not a replicated statement.
// Root removal adds GUARD (expanded into its host from the actual constant)
// and the dynamically numbered lease fence. The generic scanner does not
// expand its named paths_are helper binding or the later fence replacement;
// root_removal_composition_preserves_placeholder_order validates the actual
// complete statements for both fenced and unfenced construction instead.
const EXPECTED_FRAGMENTS: usize = 10;

/// One Rust string literal, with its escapes decoded.
#[derive(Clone)]
struct Literal {
    line: usize,
    start: usize,
    end: usize,
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
                let after = (end + terminator.len()).min(bytes.len());
                literals.push(Literal {
                    line: line_of(source, index),
                    start: index,
                    end: after,
                    text: source[content..end.min(bytes.len())].to_owned(),
                });
                blank(&mut is_code, index, after);
                index = after;
            }
            b'"' => {
                let (text, end) = decode_string(source, index);
                literals.push(Literal {
                    line: line_of(source, index),
                    start: index,
                    end,
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
            .filter(|literal| {
                literal.text.trim().is_empty()
                    || is_sql_shaped(&literal.text)
                    || literal.text.contains('?')
            })
            .map(|literal| literal.text.clone())
            .take(MAX_BINDING_ARMS)
            .collect::<Vec<_>>();
        if !arms.is_empty() {
            bindings.push((name.to_owned(), arms));
        }
    }
    bindings
}

/// Resolve only the new principal-composition family using the actual helper
/// implementations. Bindings are collected before this template, so a later
/// fixture or unrelated same-name binding cannot impersonate its authority.
fn principal_bindings(
    module: &str,
    source: &str,
    literal: &Literal,
    literals: &[Literal],
    is_code: &[bool],
    rebuilt: bool,
    source_principal: bool,
) -> Vec<(String, Vec<String>)> {
    let prefix = &source[..literal.start];
    let prior = literals
        .iter()
        .filter(|value| value.end <= literal.start)
        .cloned()
        .collect::<Vec<_>>();
    let scope_start = prefix
        .match_indices("fn ")
        .filter(|(at, _)| is_code[*at])
        .map(|(at, _)| at)
        .last()
        .unwrap_or(0);
    let scoped_literals = prior
        .iter()
        .filter(|value| value.start >= scope_start)
        .cloned()
        .map(|mut value| {
            value.start -= scope_start;
            value.end -= scope_start;
            value
        })
        .collect::<Vec<_>>();
    let mut bindings = string_bindings(
        &prefix[scope_start..],
        &scoped_literals,
        &is_code[scope_start..literal.start],
    );
    if !matches!(module, "hiqlite_sessions.rs" | "sessions.rs") {
        return bindings;
    }
    for (start, _) in prefix.match_indices("let ") {
        if start < scope_start || !is_code[start] {
            continue;
        }
        let tail = &prefix[start + 4..];
        let Some(equals) = tail.find('=') else {
            continue;
        };
        let name = tail[..equals].trim();
        let expression_start = start + 4 + equals + 1;
        // The current SQL literal may itself be inside a binding. Only
        // completed preceding bindings can provide interpolation arguments.
        let Some(expression_end) = (expression_start..prefix.len())
            .find(|&at| is_code[at] && prefix.as_bytes()[at] == b';')
        else {
            continue;
        };
        let expression = prefix[expression_start..expression_end].trim();
        // A closure definition supplies a helper, not a call with its bound
        // parameter. Its literal arguments are resolved at the actual calls.
        if expression.starts_with('|') {
            continue;
        }
        if name.starts_with('(') {
            if let Some(close) = name.find(')') {
                let names = name[1..close].split(',').map(str::trim).collect::<Vec<_>>();
                if names.len() == 2 && expression.starts_with("principal_lookup(") {
                    bindings.push((
                        names[0].to_owned(),
                        super::hiqlite_sessions::census_principal_lookup(rebuilt),
                    ));
                    continue;
                }
                let principal_tuple = matches!(
                    names.as_slice(),
                    ["owner_column", "owner"]
                        | ["columns", "values", "conflict", "owner_exists"]
                        | ["columns", "values", "conflict"]
                        | ["owner_columns", "owner_values", "conflict"]
                        | ["projection", "owner", "identity"]
                );
                if principal_tuple && expression.starts_with("if ") {
                    let arms =
                        tuple_binding_arguments(source, is_code, expression_start, expression_end);
                    assert_eq!(
                        arms.len(),
                        2,
                        "principal tuple must enumerate both observed layouts"
                    );
                    assert!(
                        arms.iter().all(|arm| arm.len() == names.len()),
                        "principal tuple arity"
                    );
                    for (index, binding) in names.iter().enumerate() {
                        let choices = arms
                            .iter()
                            .map(|arm| {
                                let argument = arm[index].trim();
                                literals
                                    .iter()
                                    .find(|value| {
                                        value.start >= expression_start
                                            && value.end <= expression_end
                                            && source[value.start..value.end] == *argument
                                    })
                                    .map(|value| value.text.clone())
                            })
                            .collect::<Option<Vec<_>>>();
                        if let Some(choices) = choices {
                            bindings.push((
                                (*binding).to_owned(),
                                vec![choices[usize::from(!rebuilt)].clone()],
                            ));
                        }
                    }
                }
            }
            continue;
        }
        if name.is_empty()
            || !name
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'_')
        {
            continue;
        }
        let expression_literals = prior
            .iter()
            .filter(|value| {
                value.start > start && value.start < start + 4 + equals + 1 + expression.len()
            })
            .collect::<Vec<_>>();
        // The examined SQLite read paths spell the same finite layout test
        // directly or through their already-read projection. Do not accept
        // arbitrary conditional bindings as principal SQL fragments.
        let sqlite_route_layout = module == "sessions.rs"
            && name == "owner_column"
            && (expression.starts_with("if route_projection(&tx)? == PRINCIPAL_ROUTE_COLS {")
                || expression.starts_with("if route_cols == PRINCIPAL_ROUTE_COLS {"));
        let mut resolved = if expression.starts_with("if source")
            || expression.starts_with("if rebuilt")
            || sqlite_route_layout
        {
            let arm = if expression.starts_with("if source") {
                if source_principal {
                    0
                } else if expression.contains("else if rebuilt") && !rebuilt {
                    2
                } else {
                    1
                }
            } else if rebuilt {
                0
            } else {
                1
            };
            expression_literals.get(arm).map(|value| value.text.clone())
        } else {
            None
        };
        if module == "hiqlite_sessions.rs" {
            // These actual read-side bindings load the same finite Local SQL
            // layout inline rather than naming a layout variable first.
            if expression == "LocalSessionSql::load(self).await?.column()" {
                resolved = super::hiqlite_sessions::census_local_principal_fragment(
                    "column", 0, "", rebuilt,
                );
            }
            for receiver in ["layout.", "ownership."] {
                if let Some(at) = expression.find(receiver) {
                    let call = &expression[at + receiver.len()..];
                    if let Some(open) = call.find('(') {
                        let method = &call[..open];
                        let args = call[open + 1..]
                            .split(')')
                            .next()
                            .unwrap_or_default()
                            .trim();
                        let parameter = if matches!(
                            method,
                            "equals" | "insert_values" | "existing_user" | "user_id_value"
                        ) {
                            args.parse()
                                .expect("principal helper index must be a literal")
                        } else {
                            0
                        };
                        let table = args.trim_matches('"');
                        resolved = super::hiqlite_sessions::census_local_principal_fragment(
                            method, parameter, table, rebuilt,
                        );
                        if source_principal {
                            let activation = &source[source
                                .find("impl ActivationSql<'_>")
                                .expect("actual Source composition")..];
                            let definition = activation
                                .split(&format!("fn {method}("))
                                .nth(1)
                                .map(|value| value.split("\n    fn ").next().unwrap_or(value));
                            if let Some(definition) = definition {
                                if definition.contains("if self.source.is_some()") {
                                    resolved = if method == "existing_user" {
                                        Some(String::new())
                                    } else {
                                        literals_and_code_mask(definition).0.first().map(|value| {
                                            value
                                                .text
                                                .replace("{parameter}", &parameter.to_string())
                                        })
                                    };
                                }
                            }
                        }
                    }
                }
            }
        } else {
            for method in ["local_owner_predicate", "live_local_session_predicate"] {
                if let Some(at) = expression.find(&format!("{method}(")) {
                    let args = &expression[at + method.len() + 1..];
                    // The final argument is a literal parameter or table name;
                    // the first may itself contain the schema-reader call.
                    let end = args.rfind(')').unwrap_or(args.len());
                    let last = args[..end]
                        .rsplit(',')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .trim_end_matches(')');
                    let parameter = if method == "local_owner_predicate" {
                        last.parse()
                            .expect("principal owner index must be a literal")
                    } else {
                        0
                    };
                    let table = last.trim_matches('"');
                    resolved = super::sqlite::census_local_principal_fragment(
                        method, parameter, table, rebuilt,
                    );
                }
            }
            if let Some((at, _)) = expression
                .match_indices("owner_predicate(")
                .find(|(at, _)| {
                    *at == 0
                        || !expression.as_bytes()[at - 1].is_ascii_alphanumeric()
                            && expression.as_bytes()[at - 1] != b'_'
                })
            {
                let argument = expression[at + "owner_predicate(".len()..]
                    .split(')')
                    .next()
                    .unwrap_or_default();
                {
                    let parameter = argument
                        .parse()
                        .expect("Source owner index must be a literal");
                    resolved = super::sqlite::census_local_principal_fragment(
                        "local_owner_predicate",
                        parameter,
                        "",
                        rebuilt,
                    );
                    if source_principal {
                        let closure = prefix
                            .rsplit("let owner_predicate")
                            .next()
                            .expect("actual Source predicate closure");
                        resolved = literals_and_code_mask(closure)
                            .0
                            .first()
                            .map(|value| value.text.replace("{parameter}", &parameter.to_string()));
                    }
                }
            }
            if let Some(value) = resolved.as_mut() {
                if expression.starts_with("format!(") {
                    if let Some(format) = prior.iter().find(|value| {
                        value.start > start
                            && value.start < start + 4 + equals + 1 + expression.len()
                    }) {
                        if let Some(alias) = format.text.strip_suffix("{}") {
                            *value = format!("{alias}{value}");
                        }
                    }
                }
            }
        }
        if let Some(value) = resolved {
            bindings.push((name.to_owned(), vec![value]));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    bindings.reverse();
    bindings.retain(|(name, _)| seen.insert(name.clone()));
    bindings
}

/// Walk just this format call's argument list. The closing outer parenthesis
/// ends the list even when its final argument has no trailing comma; later
/// SQL bindings cannot become part of a principal parameter. Strings and
/// comments use the existing code mask, so delimiters inside them are inert.
fn positional_format_arguments<'a>(
    source: &'a str,
    is_code: &[bool],
    after_literal: usize,
) -> Vec<&'a str> {
    let bytes = source.as_bytes();
    let mut at = after_literal;
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    if bytes.get(at) != Some(&b',') {
        return Vec::new();
    }
    parenthesis_arguments(source, is_code, at + 1).0
}

/// Shared delimiter walk for the actual format arguments and the six finite
/// principal-layout tuple bindings. It never reads past this parenthesis.
fn parenthesis_arguments<'a>(
    source: &'a str,
    is_code: &[bool],
    at: usize,
) -> (Vec<&'a str>, usize) {
    delimited_arguments(source, is_code, at, b')')
}

fn delimited_arguments<'a>(
    source: &'a str,
    is_code: &[bool],
    mut at: usize,
    closing: u8,
) -> (Vec<&'a str>, usize) {
    let bytes = source.as_bytes();
    let mut start = at;
    let mut depth = 0usize;
    let mut arguments = Vec::new();
    while at < bytes.len() {
        if is_code[at] {
            match bytes[at] {
                b'(' | b'[' | b'{' => depth += 1,
                byte if byte == closing && depth == 0 => {
                    if !source[start..at].trim().is_empty() {
                        arguments.push(&source[start..at]);
                    }
                    return (arguments, at);
                }
                b')' | b']' | b'}' => {
                    depth = depth.checked_sub(1).expect("balanced principal arguments")
                }
                b',' if depth == 0 => {
                    arguments.push(&source[start..at]);
                    start = at + 1;
                }
                _ => {}
            }
        }
        at += 1;
    }
    panic!("unterminated principal arguments");
}

fn tuple_binding_arguments<'a>(
    source: &'a str,
    is_code: &[bool],
    mut at: usize,
    end: usize,
) -> Vec<Vec<&'a str>> {
    let bytes = source.as_bytes();
    let mut arms = Vec::new();
    while let Some(open) = (at..end).find(|&index| is_code[index] && bytes[index] == b'{') {
        let tuple = (open + 1..end)
            .find(|&index| is_code[index] && !bytes[index].is_ascii_whitespace())
            .expect("principal tuple arm");
        assert_eq!(
            bytes[tuple], b'(',
            "principal layout branch must return an explicit tuple"
        );
        let (arguments, close) = parenthesis_arguments(source, is_code, tuple + 1);
        assert!(close < end, "tuple remains in its binding");
        arms.push(arguments);
        at = close + 1;
    }
    arms
}

/// Evaluate only the reviewed path-set constructor with literal backend
/// placeholders. A recognized call with a new argument shape must be reviewed;
/// replacing it with the neutral token would erase its binding obligations.
fn library_path_call(expression: &str) -> Option<String> {
    let expression = expression.trim();
    let (_, expression_code) = literals_and_code_mask(expression);
    let known = expression
        .match_indices("library_paths_are_sql")
        .any(|(at, _)| {
            expression_code[at]
                && expression[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                && expression[at + "library_paths_are_sql".len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        });
    if !known {
        return None;
    }
    static CALL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let grammar = CALL.get_or_init(|| {
        regex::Regex::new(
            r#"^(?:crate::store|super)::library_paths_are_sql\s*\(\s*"([?$][1-9][0-9]*)"\s*,\s*"([?$][1-9][0-9]*)"\s*\)$"#,
        )
        .expect("literal library path constructor grammar")
    });
    let arguments = grammar
        .captures(expression)
        .expect("library path SQL constructor requires reviewed literal placeholder arguments");
    Some(super::library_paths_are_sql(&arguments[1], &arguments[2]))
}

/// The observed publication wrapper returns result counts after executing its
/// explicit SQL/params tuples. Only a helper inside an independently modeled
/// SQL format is admitted; helpers in routing, parameters or unknown wrappers
/// still fail the ordinary constructor grammar.
fn completed_library_path_publication(
    source: &str,
    start: usize,
    end: usize,
    literals: &[Literal],
    is_code: &[bool],
) -> bool {
    static CALL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let grammar = CALL.get_or_init(|| {
        regex::Regex::new(r"^\s*self\s*\.\s*atomic_publication\s*\(")
            .expect("completed publication receiver grammar")
    });
    let Some(call) = grammar.find(&source[start..end]) else {
        return false;
    };
    let open = start + call.end() - 1;
    let (arguments, close) = parenthesis_arguments(source, is_code, open + 1);
    if close >= end
        || arguments.len() != 3
        || arguments[0].trim() != "lease"
        || arguments[1].trim() != "replacement"
        || source[close + 1..end]
            .split_whitespace()
            .collect::<String>()
            != ".await?"
    {
        return false;
    }
    let vector = arguments[2].trim();
    if !vector.starts_with("vec![") {
        return false;
    }
    let vector_start =
        open + 1 + arguments[0].len() + 1 + arguments[1].len() + 1 + arguments[2].len()
            - arguments[2].trim_start().len();
    let (entries, vector_close) = delimited_arguments(source, is_code, vector_start + 5, b']');
    if entries.is_empty()
        || vector_close >= close
        || !source[vector_close + 1..vector_start + vector.len()]
            .trim()
            .is_empty()
    {
        return false;
    }
    let mut at = vector_start + 5;
    let mut formats = Vec::new();
    for entry in entries {
        let tuple_start = at + entry.len() - entry.trim_start().len();
        let tuple_end = tuple_start + entry.trim().len();
        if !entry.trim().starts_with('(') {
            return false;
        }
        let (fields, tuple_close) = parenthesis_arguments(source, is_code, tuple_start + 1);
        if fields.len() != 2
            || tuple_close >= tuple_end
            || !source[tuple_close + 1..tuple_end].trim().is_empty()
        {
            return false;
        }
        let sql_start = tuple_start + 1 + fields[0].len() - fields[0].trim_start().len();
        let sql_end = sql_start + fields[0].trim().len();
        if fields[0].trim().starts_with("format!(") {
            if !completed_library_path_statement(source, sql_start, sql_end, literals, is_code) {
                return false;
            }
            formats.push((sql_start, sql_end));
        } else {
            let Some(sql) = literals.iter().find(|literal| literal.start == sql_start) else {
                return false;
            };
            if sql.end > sql_end
                || !is_sql_candidate(&sql.text)
                || source[sql.end..sql_end].trim() != ".to_owned()"
            {
                return false;
            }
        }
        let params_start =
            tuple_start + 1 + fields[0].len() + 1 + fields[1].len() - fields[1].trim_start().len();
        if !fields[1].trim().starts_with("params!(") {
            return false;
        }
        let (_, params_close) = parenthesis_arguments(source, is_code, params_start + 8);
        if params_close >= tuple_close
            || !source[params_close + 1..params_start + fields[1].trim().len()]
                .trim()
                .is_empty()
        {
            return false;
        }
        at += entry.len() + 1;
    }
    let helpers = source[start..end]
        .match_indices("library_paths_are_sql")
        .filter(|(at, _)| is_code[start + at])
        .map(|(at, _)| start + at)
        .collect::<Vec<_>>();
    !helpers.is_empty()
        && helpers.iter().all(|at| {
            formats
                .iter()
                .any(|(begin, finish)| *at >= *begin && *at < *finish)
        })
}

/// A completed reviewed SQL operation is not a predicate binding for a later
/// statement. Admit only the exact format expression, or the first argument of
/// the observed SQLite query_row wrapper. Every helper occurrence must belong
/// to that independently modeled format expression.
fn completed_library_path_statement(
    source: &str,
    start: usize,
    end: usize,
    literals: &[Literal],
    is_code: &[bool],
) -> bool {
    if completed_library_path_publication(source, start, end, literals, is_code) {
        return true;
    }
    let expression = &source[start..end];
    let trimmed = expression.trim_start();
    let begin = start + expression.len() - trimmed.len();
    let (format_start, format_end) = if trimmed.starts_with("format!(") {
        (begin, end)
    } else if let Some(rest) = trimmed.strip_prefix("conn.query_row") {
        let rest = rest.trim_start();
        if !rest.starts_with('(') {
            return false;
        }
        let open = end - rest.len();
        let (arguments, close) = parenthesis_arguments(source, is_code, open + 1);
        if close >= end || arguments.len() != 3 || source[close + 1..end].trim() != "?" {
            return false;
        }
        let first = arguments[0];
        let argument = first.trim();
        if !argument.starts_with("&format!(") {
            return false;
        }
        let argument_start = open + 1 + first.len() - first.trim_start().len();
        (argument_start + 1, argument_start + argument.len())
    } else {
        return false;
    };
    let (arguments, close) =
        parenthesis_arguments(source, is_code, format_start + "format!(".len());
    if close >= format_end || !source[close + 1..format_end].trim().is_empty() {
        return false;
    }
    let Some(first) = arguments.first() else {
        return false;
    };
    let literal_start = format_start + "format!(".len() + first.len() - first.trim_start().len();
    let Some(own_sql) = literals
        .iter()
        .find(|literal| literal.start == literal_start)
    else {
        return false;
    };
    if &source[own_sql.start..own_sql.end] != first.trim() || !is_sql_shaped(&own_sql.text) {
        return false;
    }
    let helper_occurrences = expression
        .match_indices("library_paths_are_sql")
        .filter(|(at, _)| is_code[start + at])
        .map(|(at, _)| start + at)
        .collect::<Vec<_>>();
    if helper_occurrences.is_empty()
        || helper_occurrences
            .iter()
            .any(|at| *at < format_start || *at >= close)
    {
        return false;
    }
    let (_, _, resolved) = library_path_template(source, own_sql, literals, is_code);
    resolved
        && (is_sql_candidate(&own_sql.text)
            || composed_library_path_candidate(source, own_sql, literals, is_code))
}

/// Resolve the actual helper in positional format arguments and local named
/// bindings. The constructor's output is judged by the ordinary SQL validators.
fn library_path_template(
    source: &str,
    literal: &Literal,
    literals: &[Literal],
    is_code: &[bool],
) -> (String, Vec<(String, Vec<String>)>, bool) {
    let mut template = literal.text.clone();
    let mut bindings = string_bindings(source, literals, is_code);
    let mut resolved = false;
    if !is_sql_shaped(&template) {
        return (template, bindings, resolved);
    }
    let formatted = source[..literal.start].trim_end().ends_with("format!(");
    let arguments = if formatted {
        positional_format_arguments(source, is_code, literal.end)
    } else {
        Vec::new()
    };
    if formatted {
        let helper_arguments = arguments
            .iter()
            .map(|expression| library_path_call(expression))
            .collect::<Vec<_>>();
        let positions = template
            .match_indices("{}")
            .filter(|(at, _)| {
                template.as_bytes().get(at.wrapping_sub(1)) != Some(&b'{')
                    && template.as_bytes().get(at + 2) != Some(&b'}')
            })
            .map(|(at, _)| at)
            .collect::<Vec<_>>();
        if helper_arguments.iter().any(Option::is_some) {
            assert_eq!(
                positions.len(),
                arguments.len(),
                "library path SQL accepts only anonymous positional helper arguments"
            );
            assert!(
                !template
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'{' && (pair[1].is_ascii_digit() || pair[1] == b':')),
                "library path SQL accepts only anonymous positional helper arguments"
            );
        }
        if !positions.is_empty() {
            let replacements = positions
                .iter()
                .zip(helper_arguments)
                .enumerate()
                .filter_map(|(index, (at, helper))| {
                    helper.map(|value| {
                        let name = format!("census_library_path_{index}");
                        bindings.push((name.clone(), vec![value]));
                        (*at, name)
                    })
                })
                .collect::<Vec<_>>();
            for (at, name) in replacements.into_iter().rev() {
                template.replace_range(at..at + 2, &format!("{{{name}}}"));
                resolved = true;
            }
        }
    }
    let function_start = source[..literal.start]
        .match_indices("fn ")
        .filter(|(at, _)| is_code[*at])
        .map(|(at, _)| at)
        .last()
        .unwrap_or(0);
    let context = &source[function_start..literal.start];
    static BINDING: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let binding = BINDING.get_or_init(|| {
        regex::Regex::new(
            r"\blet\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)(?:\s*:[^=;]+)?\s*=\s*([^;]*);",
        )
        .expect("local library path binding grammar")
    });
    let declarations = binding
        .captures_iter(context)
        .filter(|capture| is_code[function_start + capture.get(0).expect("binding match").start()])
        .collect::<Vec<_>>();
    let block_at = |end: usize| {
        let mut blocks = Vec::new();
        for (at, byte) in source
            .as_bytes()
            .iter()
            .enumerate()
            .take(end)
            .skip(function_start)
        {
            if is_code[at] {
                match byte {
                    b'{' => blocks.push(at),
                    b'}' => {
                        blocks.pop();
                    }
                    _ => {}
                }
            }
        }
        blocks
    };
    for capture in &declarations {
        let at = function_start + capture.get(0).expect("binding match").start();
        if !is_code[at] {
            continue;
        }
        let expression = capture.get(2).expect("binding expression");
        if completed_library_path_statement(
            source,
            function_start + expression.start(),
            function_start + expression.end(),
            literals,
            is_code,
        ) {
            continue;
        }
        if let Some(value) = library_path_call(&capture[2]) {
            assert_eq!(
                declarations
                    .iter()
                    .filter(|other| other[1] == capture[1])
                    .count(),
                1,
                "library path SQL requires one immutable direct binding without shadows"
            );
            let name = &capture[1];
            let mentions = context
                .match_indices(name)
                .filter(|(index, _)| {
                    is_code[function_start + index]
                        && context[..*index]
                            .chars()
                            .next_back()
                            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                        && context[index + name.len()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                })
                .count();
            assert_eq!(
                mentions, 1,
                "library path SQL binding cannot be shadowed, reassigned or indirectly used"
            );
            let declaration = capture.get(0).expect("binding declaration").as_str();
            let header = declaration
                .split_once('=')
                .expect("binding equals")
                .0
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                header,
                format!("let {}", &capture[1]),
                "library path SQL requires one immutable direct untyped binding"
            );
            if !template.contains(&format!("{{{}}}", &capture[1])) {
                continue;
            }
            assert_eq!(
                block_at(at),
                block_at(literal.start),
                "library path SQL binding must belong to the enclosing live lexical block"
            );
            assert!(
                arguments.is_empty(),
                "library path SQL local binding cannot have explicit format arguments"
            );
            bindings.retain(|(name, _)| name != &capture[1]);
            bindings.push((capture[1].to_owned(), vec![value]));
            resolved = true;
        }
    }
    (template, bindings, resolved)
}

fn composed_library_path_candidate(
    source: &str,
    literal: &Literal,
    literals: &[Literal],
    is_code: &[bool],
) -> bool {
    is_sql_shaped(&literal.text) && library_path_template(source, literal, literals, is_code).2
}

fn principal_templates(
    module: &str,
    source: &str,
    literal: &Literal,
    literals: &[Literal],
    is_code: &[bool],
    constants: &[(String, String)],
) -> Vec<String> {
    if !matches!(module, "hiqlite_sessions.rs" | "sessions.rs") {
        let (template, bindings, _) = library_path_template(source, literal, literals, is_code);
        return resolve_template(&template, constants, &bindings);
    }
    assert!(
        !library_path_template(source, literal, literals, is_code).2,
        "library path SQL in principal composition requires explicit joint modeling"
    );
    let mut function_start = 0;
    for (at, _) in source[..literal.start].match_indices("fn ") {
        if is_code[at] {
            function_start = at;
        }
    }
    let context = &source[function_start..literal.start];
    let source_activation = context.contains("ActivationSql {")
        || context.contains("let owner_predicate=|parameter| if source");
    let modes = if source_activation {
        &[(false, false), (true, false), (true, true)][..]
    } else {
        &[(false, false), (true, false)][..]
    };
    static PARAMETERS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let parameters =
        PARAMETERS.get_or_init(|| regex::Regex::new(r"\$(\d+)").expect("parameter grammar"));
    let mut variants = Vec::new();
    for &(rebuilt, source_principal) in modes {
        let mut bindings = principal_bindings(
            module,
            source,
            literal,
            literals,
            is_code,
            rebuilt,
            source_principal,
        );
        let mut template = literal.text.clone();
        // Only these session composition sites pass layout helpers positionally.
        // Evaluate their real output before numbering or ordering placeholders.
        let positional_at = |template: &str| {
            template.match_indices("{}").find_map(|(at, _)| {
                (template.as_bytes().get(at.wrapping_sub(1)) != Some(&b'{')
                    && template.as_bytes().get(at + 2) != Some(&b'}'))
                .then_some(at)
            })
        };
        let formatted = source[..literal.start].trim_end().ends_with("format!(");
        // Plain SQL JSON braces and SQL arrays are not format argument lists.
        let argument_list = if formatted && positional_at(&template).is_some() {
            positional_format_arguments(source, is_code, literal.end)
        } else {
            Vec::new()
        };
        let mut arguments = argument_list.into_iter();
        let mut argument_index = 0;
        while let Some(at) = formatted.then(|| positional_at(&template)).flatten() {
            let expression = arguments
                .next()
                .expect("positional SQL format argument")
                .trim();
            let Some(receiver) = ["layout.", "ownership."]
                .into_iter()
                .find_map(|value| expression.strip_prefix(value))
            else {
                break;
            };
            let Some(open) = receiver.find('(') else {
                break;
            };
            let method = &receiver[..open];
            let argument = receiver[open + 1..]
                .strip_suffix(')')
                .expect("complete principal helper call")
                .trim();
            let parameter = if matches!(
                method,
                "equals" | "insert_values" | "existing_user" | "user_id_value"
            ) {
                argument
                    .parse()
                    .expect("positional principal index must be a literal")
            } else {
                assert!(
                    argument.is_empty(),
                    "unsupported positional principal arguments"
                );
                0
            };
            let value = super::hiqlite_sessions::census_local_principal_fragment(
                method, parameter, "", rebuilt,
            )
            .expect("unsupported positional principal helper must be reviewed");
            let name = format!("census_arg_{argument_index}");
            template.replace_range(at..at + 2, &format!("{{{name}}}"));
            bindings.push((name, vec![value]));
            argument_index += 1;
        }
        for name in template
            .split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
        {
            if (matches!(
                name,
                "owner"
                    | "request_owner"
                    | "request_update_owner"
                    | "live_route"
                    | "live_route_sql"
                    | "live_session"
                    | "live_alias"
                    | "local_inventory"
                    | "existing_user"
            ) || name.starts_with("owner_")
                || name.starts_with("extra_")
                || name.starts_with("user_value_")
                || name.starts_with("principal_"))
                && !bindings.iter().any(|(binding, _)| binding == name)
                && !constants.iter().any(|(binding, _)| binding == name)
            {
                panic!(
                    "{module}:{} unresolved principal fragment {name}",
                    literal.line
                );
            }
        }
        for mut value in resolve_template(&template, constants, &bindings) {
            if source_principal
                && module == "hiqlite_sessions.rs"
                && context.starts_with("fn activate_with_authority(")
                && context.contains("let mut statements = vec![")
                && is_statement(&value)
            {
                // This exact production branch maps every vector member through
                // ordered_source_lifecycle_statement before the Raft proposal.
                // Use its real canonicalizer, including rejection of holes or
                // unused bindings; raw pre-remap introduction order is not wire SQL.
                let max = parameters
                    .captures_iter(&value)
                    .map(|capture| capture[1].parse::<usize>().expect("parameter index"))
                    .max()
                    .unwrap_or(0);
                value =
                    super::sharing::ordered(&value, vec![super::sharing::Value::Integer(0); max])
                        .expect("actual Source lifecycle canonicalization")
                        .0;
            }
            if !variants.contains(&value) {
                variants.push(value);
            }
        }
    }
    variants
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

/// Validate SQLite's numeric `?N` dialect and return its highest index.
///
/// Unlike `$N`, first appearance does not define the binding slot. Requiring
/// the complete `1..=max` set prevents a gap from silently shifting a
/// `params!` list written in appearance order, while rejecting a mixed bare
/// `?` keeps the assignment independent of the text that precedes it.
pub(super) fn validate_sqlite_placeholders(sql: &str) -> Result<usize, String> {
    let bytes = sql.as_bytes();
    let mut index = 0_usize;
    let mut numbered = std::collections::BTreeSet::new();
    let mut bare = false;
    while index < bytes.len() {
        match bytes[index] {
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index = sql[index..]
                    .find('\n')
                    .map_or(bytes.len(), |offset| index + offset + 1);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = sql[index + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |offset| index + 2 + offset + 2);
            }
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == quote {
                        if bytes.get(index + 1) == Some(&quote) {
                            index += 2;
                        } else {
                            index += 1;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
            }
            b'[' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b']' {
                        if bytes.get(index + 1) == Some(&b']') {
                            index += 2;
                        } else {
                            index += 1;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
            }
            b'?' => {
                let start = index + 1;
                index = start;
                while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                    index += 1;
                }
                if index == start {
                    bare = true;
                } else {
                    let number = sql[start..index]
                        .parse::<usize>()
                        .map_err(|error| format!("invalid ?N placeholder: {error}"))?;
                    if number == 0 {
                        return Err("placeholder indices start at ?1, not ?0".to_owned());
                    }
                    numbered.insert(number);
                }
            }
            _ => index += 1,
        }
    }
    if bare && !numbered.is_empty() {
        return Err("bare ? cannot be mixed with indexed ?N placeholders".to_owned());
    }
    let max = numbered.last().copied().unwrap_or(0);
    if let Some(missing) = (1..=max).find(|number| !numbered.contains(number)) {
        return Err(format!(
            "indexed placeholders must cover ?1..?{max}; missing ?{missing}"
        ));
    }
    Ok(max)
}

/// Literal binding arity when the SQL and arguments share one Rust statement.
///
/// A semicolon before `params![…]`/`&[…]` means the SQL escaped into a local or
/// prepared statement and is deliberately counted as unchecked. This is a
/// conservative syntactic proof: it never guesses across statements.
fn same_statement_binding_arity(
    source: &str,
    is_code: &[bool],
    literal_end: usize,
) -> Option<usize> {
    let mut candidate = None;
    let bytes = source.as_bytes();
    let mut cursor = literal_end;
    while cursor < bytes.len() {
        if !is_code.get(cursor).copied().unwrap_or(false) {
            cursor += 1;
            continue;
        }
        if matches!(bytes[cursor], b';' | b'}') {
            break;
        }
        let prefix_len = if source[cursor..].starts_with("params![") {
            Some("params![".len())
        } else if source[cursor..].starts_with("&[") {
            Some("&[".len())
        } else if bytes[cursor] == b'[' {
            Some(1)
        } else {
            None
        };
        if let Some(prefix_len) = prefix_len {
            candidate = Some((cursor, prefix_len));
            break;
        }
        cursor += 1;
    }
    let (start, prefix_len) = candidate?;
    let open = start + prefix_len - 1;
    let mut index = open + 1;
    let mut square = 1_usize;
    let mut round = 0_usize;
    let mut curly = 0_usize;
    let mut values = 0_usize;
    let mut current_value = false;
    while index < bytes.len() {
        if !is_code.get(index).copied().unwrap_or(false) {
            index += 1;
            continue;
        }
        match bytes[index] {
            b'[' => square += 1,
            b']' => {
                square = square.saturating_sub(1);
                if square == 0 {
                    return Some(values + usize::from(current_value));
                }
            }
            b'(' => round += 1,
            b')' => round = round.saturating_sub(1),
            b'{' => curly += 1,
            b'}' => curly = curly.saturating_sub(1),
            b',' if square == 1 && round == 0 && curly == 0 => {
                if current_value {
                    values += 1;
                    current_value = false;
                }
            }
            byte if square == 1 && round == 0 && curly == 0 && !byte.is_ascii_whitespace() => {
                current_value = true;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn is_sqlite_candidate(text: &str) -> bool {
    text.contains('?') && is_sql_shaped(text)
}

/// Current number of SQLite statement variants whose binding list is built in
/// another statement or dynamically. This is measured by the census and may
/// only fall unless a reviewer accepts a new non-local binding shape.
///
/// 90 -> 91 for exactly one reviewed site: the `SELECT` prepared by
/// `files_missing_field_order` in `sqlite/media.rs`, whose `?1`/`?2` are bound
/// by the `query_map` on the next statement, so no local `params!` sits beside
/// the literal for the census to count. Two placeholders, two values. It is
/// the same shape `files_missing_video_codec_tag` immediately above it already
/// contributes, and the census was re-measured site by site against
/// `origin/main`: that one literal is the entire difference between the two
/// sets, so nothing else changed binding shape in this merge.
///
/// 91 -> 92 for exactly one more reviewed site of that same shape: the
/// `SELECT` prepared by `files_missing_luminance` in `sqlite/media.rs`, whose
/// `?1`/`?2` are bound by the `query_map` on the next statement. The census
/// was re-measured against the merged tree and that one literal is the whole
/// difference; the luminance `UPDATE` beside it binds locally and is checked.
///
/// 92 -> 93 on this branch for one more site of that same shape:
/// `list_tokens_for_user` in `sqlite/users.rs` prepares the device-inventory
/// `SELECT` once and binds `?1`/`?2` in the `query_map` on the next statement,
/// so its arity cannot be paired syntactically even though the numbered
/// placeholders themselves are still covered by this census. The two entries
/// main contributed (field order, luminance) and this one are the whole
/// difference between the pre-merge sets; no other literal changed shape.
///
/// +2 on K-05's branch (93 -> 95 before main's two subtitle-source sites
/// below, so 97 with them), re-measured site by site against `origin/main`
/// (the census printed every unchecked literal on both trees): the query-plan
/// capture needs each hot statement's SQL in a binding it can hand to
/// `trace_statement` before running it, which takes three statements that
/// bound inline out of the census's reach — `list_top_items_in_genre`'s
/// `COUNT(*)` (`?1`/`?2`, two values) and `item_by_external_id` (`?1`-`?3`,
/// three) in `sqlite/media.rs`, and `authenticate_token`'s read in
/// `sqlite/users.rs` (a `const SQL` beside its `query_row`). `recently_added`
/// left the directory for `sql_source` and takes its unchecked site with it.
/// Each of the three runs in the SQLite contracts, where rusqlite refuses a
/// wrong parameter count at run time.
// Subtitle-source discovery prepares its shared candidate query before the
// `query_map` binding, and ready retirement composes its shared
// uncovered-ordinal predicate before binding in the next statement.
//
// 97 -> 96 on K-05 M5's branch: `item_by_external_id` left
// `sqlite/media.rs` for `sql_source` (one statement for both dialects, the
// union spelling its partial indexes need) and takes its unchecked
// `?1`-`?3` site with it, as `recently_added` did. `list_top_items_in_genre`'s
// count and page moved into `library_page_statements` in the same file and
// bind exactly as before, so their sites are unchanged.
// Common queue execution removes three legacy prepared/bound SQL sites.
// The new bridge uses one JSON argument rather than per-field binding lists.
// E2 retires the old separately prepared manifest-candidate query.
// 92 -> 91: the separately prepared fragment-prune candidate SELECT moved
// unchanged to the shared FRAGMENT_PRUNE_CANDIDATES constant. Its two values
// remain bound by query_map; full-schema contracts execute that exact constant
// on both backends. The terminal DELETE previously bound inline and contributed
// no unchecked site, so it does not change this count.
// 91 -> 92: analysis_reconciliation_page prepares its keyset SELECT before
// query_map binds `after` and `limit` to ?1 and ?2. The shared store contract
// analysis_reconciliation_preserves_work_and_fences_changed_requests executes
// the paginated query; the separate preparation keeps its arity out of this
// same-statement scanner's reach.
// J3 removes three separately prepared watch-tree loop statements; the shared
// atomic manual SQL is exercised by both-backend revision contracts.
// 89 -> 92: three continuous-quality SQLite statements bind their values
// away from the literal's own statement, where this scanner cannot count them.
// They are the continuous-quality ledger, reservation and cancellation-receipt
// paths; the continuous_quality_ledger_* and quality_cancellation_* store
// contracts execute every one of them on both backends.
// 92 -> 93: K-05's plan pin moves `watch_rollup`'s statement into
// `watch_rollup_sql()` so `watch_rollup_plans_do_not_depend_on_statistics` can
// plan the exact text; `query_row` binds `?1`/`?2` beside the call, out of
// this scanner's reach. `marking_a_show_reaches_every_episode_under_it` and
// `a_page_of_containers_rolls_up_in_one_pass_and_agrees_with_the_single_walk`
// execute it.
// 93 -> 102: nine reviewed statement variants introduced by sharing's
// principal/identity composition. Three are additional rebuilt-layout variants
// of separately bound SELECTs: sessions::desired_within (?1..?2, two values),
// expired_media_sessions (?1..?6, six), and owned_media_sessions (?1..?4, four).
// Six are the two actual inline conditional INSERT arms in each of
// sessions::claim_media_session_request (?1..?7, seven values),
// media::insert_item and publication::insert_item_fenced (?1..?8, eight each).
// Those six bind the shared params! immediately after the conditional, but this
// deliberately conservative scanner stops at the arm's closing brace. Every
// branch's placeholders remain validated; no source statement is exempted.
// 102 -> 103: files_with_unsignalled_transfer prepares its ?1/?2 candidate
// SELECT before query_map binds after_id and limit.max(0). Its exact-fencing
// store contract executes that page and the matching conditional update.
// Also 103: publication::remove_library_files prepares its guarded DELETE
// separately, then binds [id, library_id] to ?1/?2 for each admitted file.
// library_path_removal_runs_through_dyn_store executes that exact operation.
// The path-set SELECTs are composed through the real helper above and have
// inline two-value bindings; those checked statements add no exception.
// 103 -> 107: annotations.rs prepares four statements separately from their
// query_map bindings: rating rows (?1), person/item flags (?1..?2), list page
// (?1..?4), and JSON-filtered membership (?1..?3). Each binds exactly those
// values in the following query_map call; registering this previously omitted
// source expands coverage without exempting any SQL or changing scanner rules.
const EXPECTED_UNCHECKED_SQLITE_ARITY: usize = 107;

#[test]
fn every_sqlite_placeholder_and_local_binding_arity_is_valid() {
    let mut offenders = Vec::new();
    let mut scanned = 0_usize;
    let mut unchecked = Vec::new();
    for (name, source) in SQLITE_SOURCES {
        let (literals, is_code) = literals_and_code_mask(source);
        let test_ranges = test_item_ranges(source, &is_code);
        let constants = constants_for(name, source, &literals);
        for literal in &literals {
            if test_ranges
                .iter()
                .any(|(start, end)| literal.start >= *start && literal.start < *end)
                || !(is_sqlite_candidate(&literal.text)
                    || composed_library_path_candidate(source, literal, &literals, &is_code))
            {
                continue;
            }
            for statement in
                principal_templates(name, source, literal, &literals, &is_code, &constants)
            {
                if !is_statement(&statement) {
                    continue;
                }
                scanned += 1;
                match validate_sqlite_placeholders(&statement) {
                    Ok(max) if max > 0 => {
                        if let Some(arity) =
                            same_statement_binding_arity(source, &is_code, literal.end)
                        {
                            if arity != max {
                                offenders.push(format!(
                                    "{name}:{}: binds {arity} values for ?1..?{max}\n    {}",
                                    literal.line,
                                    statement.split_whitespace().collect::<Vec<_>>().join(" ")
                                ));
                            }
                        } else {
                            unchecked.push(format!(
                                "{name}:{}: {}",
                                literal.line,
                                statement.split_whitespace().collect::<Vec<_>>().join(" ")
                            ));
                        }
                    }
                    Ok(_) => {}
                    Err(error) => offenders.push(format!(
                        "{name}:{}: {error}\n    {}",
                        literal.line,
                        statement.split_whitespace().collect::<Vec<_>>().join(" ")
                    )),
                }
            }
        }
    }
    assert!(
        scanned > 300,
        "the SQLite census found only {scanned} statements; the scanner is broken"
    );
    assert!(
        offenders.is_empty(),
        "SQLite indexed placeholders or local binding arities are invalid:\n{}",
        offenders.join("\n")
    );
    assert_eq!(
        unchecked.len(),
        EXPECTED_UNCHECKED_SQLITE_ARITY,
        "the statically unchecked SQLite arity set changed; inspect every new site:\n{}",
        unchecked.join("\n")
    );
}

mod source_inventory {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/rust_source_inventory.rs"
    ));
}

#[test]
fn sqlite_module_list_matches_the_directory() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/sqlite");

    let mut present = source_inventory::production_files(&directory, &[&directory.join("mod.rs")])
        .into_iter()
        .map(|path| {
            path.strip_prefix(&directory)
                .expect("SQLite source file")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>();
    present.sort();
    let mut censused = SQLITE_SOURCES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<Vec<_>>();
    censused.sort();
    assert_eq!(
        present, censused,
        "every SQLite store slice must be censused"
    );
    for (name, source) in SQLITE_SOURCES {
        let on_disk = std::fs::read_to_string(directory.join(name))
            .unwrap_or_else(|error| panic!("{name} is readable: {error}"));
        assert_eq!(
            on_disk.len(),
            source.len(),
            "{name} is paired with the wrong source"
        );
    }
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
    for (name, source) in STORE_SOURCES
        .iter()
        .chain(SHARED_CONSTANT_SOURCES.iter().filter(|(name, _)| {
            matches!(
                *name,
                "../live_tv_resource.rs"
                    | "continuous_family.rs"
                    | "quality_cancellation.rs"
                    | "quality_ledger.rs"
            )
        }))
    {
        let (literals, is_code) = literals_and_code_mask(source);
        let test_ranges = test_item_ranges(source, &is_code);
        let constants = constants_for(name, source, &literals);
        for literal in &literals {
            if test_ranges
                .iter()
                .any(|(start, end)| literal.start >= *start && literal.start < *end)
            {
                continue;
            }
            if !(is_sql_candidate(&literal.text)
                || composed_library_path_candidate(source, literal, &literals, &is_code))
            {
                continue;
            }
            let mut judged = false;
            for statement in
                principal_templates(name, source, literal, &literals, &is_code, &constants)
            {
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
        composed_library_path_candidate, is_sql_candidate, is_sql_shaped, is_statement,
        literals_and_code_mask, positional_format_arguments, principal_templates, resolve_template,
        same_statement_binding_arity, string_bindings, string_constants, test_item_ranges,
        validate_sql, validate_sqlite_placeholders,
    };

    #[test]
    fn library_path_positional_composition_preserves_sequential_bindings() {
        for (paths, valid) in [("$4", true), ("$5", false)] {
            let source = format!(
                r#"fn query() {{ let sql = format!("INSERT INTO roots SELECT $1, $2 WHERE $3 AND {{}} AND $5 = $5", crate::store::library_paths_are_sql("$1", "{paths}")); }}"#
            );
            let (literals, mask) = literals_and_code_mask(&source);
            let sql = literals
                .iter()
                .find(|value| value.text.starts_with("INSERT"))
                .expect("INSERT template fixture");
            let variants = principal_templates(
                "hiqlite_publication.rs",
                &source,
                sql,
                &literals,
                &mask,
                &[],
            );
            assert_eq!(variants.len(), 1);
            assert!(variants[0].contains(&crate::store::library_paths_are_sql("$1", paths)));
            assert_eq!(validate_sql(&variants[0]).is_ok(), valid);
        }
    }

    #[test]
    fn library_path_named_composition_preserves_sequential_bindings() {
        for (paths, valid) in [("$2", true), ("$3", false)] {
            let source = format!(
                r#"fn query() {{ let paths_are = super::library_paths_are_sql("$1", "{paths}"); let sql = format!("INSERT INTO guards SELECT $1 WHERE {{paths_are}}"); }}"#
            );
            let (literals, mask) = literals_and_code_mask(&source);
            let sql = literals
                .iter()
                .find(|value| value.text.starts_with("INSERT"))
                .expect("INSERT template fixture");
            let variants =
                principal_templates("hiqlite_media.rs", &source, sql, &literals, &mask, &[]);
            assert_eq!(variants.len(), 1);
            assert!(variants[0].contains(&crate::store::library_paths_are_sql("$1", paths)));
            assert_eq!(validate_sql(&variants[0]).is_ok(), valid);
        }
    }

    #[test]
    fn composed_sqlite_path_select_is_checked_with_inline_arity() {
        for (bindings, arity) in [("library, paths", 2), ("library", 1)] {
            let source = format!(
                r#"fn query() {{ conn.query_row(&format!("SELECT {{}}", crate::store::library_paths_are_sql("?1", "?2")), params![{bindings}], |row| row.get(0)); }}"#
            );
            let (literals, mask) = literals_and_code_mask(&source);
            let sql = literals
                .iter()
                .find(|value| value.text.starts_with("SELECT"))
                .expect("SELECT template fixture");
            assert!(composed_library_path_candidate(
                &source, sql, &literals, &mask
            ));
            let variants =
                principal_templates("publication.rs", &source, sql, &literals, &mask, &[]);
            let maximum =
                validate_sqlite_placeholders(&variants[0]).expect("valid indexed SQLite predicate");
            assert_eq!(maximum, 2);
            assert_eq!(
                same_statement_binding_arity(&source, &mask, sql.end),
                Some(arity)
            );
            assert_eq!(maximum == arity, bindings == "library, paths");
        }
    }

    #[test]
    #[should_panic(
        expected = "library path SQL constructor requires reviewed literal placeholder arguments"
    )]
    fn library_path_constructor_refuses_unreviewed_dynamic_arguments() {
        let source = r#"fn query() { let sql = format!("SELECT {}", super::library_paths_are_sql(library, "$2")); }"#;
        let (literals, mask) = literals_and_code_mask(source);
        let sql = literals
            .iter()
            .find(|value| value.text.starts_with("SELECT"))
            .expect("SELECT template fixture");
        let _ = composed_library_path_candidate(source, sql, &literals, &mask);
    }

    #[test]
    fn composed_replicated_path_select_is_admitted_and_checked() {
        for (paths, valid) in [("$2", true), ("$3", false)] {
            let source = format!(
                r#"fn query() {{ let sql = format!("SELECT {{}}", super::library_paths_are_sql("$1", "{paths}")); }}"#
            );
            let (literals, mask) = literals_and_code_mask(&source);
            let sql = literals
                .iter()
                .find(|value| value.text.starts_with("SELECT"))
                .expect("SELECT fixture");
            assert!(composed_library_path_candidate(
                &source, sql, &literals, &mask
            ));
            let variants =
                principal_templates("hiqlite_media.rs", &source, sql, &literals, &mask, &[]);
            assert_eq!(validate_sql(&variants[0]).is_ok(), valid);
        }
    }

    #[test]
    fn library_path_named_binding_refuses_shadows_and_closed_blocks() {
        for prefix in [
            r#"let paths_are = super::library_paths_are_sql("$1", "$2"); let paths_are = "$3 = $3";"#,
            r#"let paths_are = super::library_paths_are_sql("$1", "$2"); let (paths_are,) = ("$3 = $3",);"#,
            r#"{ let paths_are = super::library_paths_are_sql("$1", "$2"); }"#,
            r#"let mut paths_are = super::library_paths_are_sql("$1", "$2");"#,
            r#"let paths_are: String = super::library_paths_are_sql("$1", "$2");"#,
            r#"let paths_are = super::library_paths_are_sql("$1", "$2"); alter(&mut paths_are);"#,
            r#"let paths_are = super::library_paths_are_sql("$1", "$2"); let alias = paths_are;"#,
        ] {
            let source =
                format!(r#"fn query() {{ {prefix} let sql = format!("SELECT {{paths_are}}"); }}"#);
            let refused = std::panic::catch_unwind(|| {
                let (literals, mask) = literals_and_code_mask(&source);
                let sql = literals
                    .iter()
                    .find(|value| value.text.starts_with("SELECT"))
                    .expect("SELECT fixture");
                composed_library_path_candidate(&source, sql, &literals, &mask)
            });
            assert!(
                refused.is_err(),
                "unsupported local binding must fail closed: {prefix}"
            );
        }
    }

    #[test]
    fn library_path_constructor_refuses_wrapped_and_spaced_dynamic_calls() {
        for argument in [
            r#"super::library_paths_are_sql (library, "$2")"#,
            r#"(super::library_paths_are_sql("$1", "$2"))"#,
            r#"super::library_paths_are_sql("$1", paths)"#,
        ] {
            let source =
                format!(r#"fn query() {{ let sql = format!("SELECT {{}}", {argument}); }}"#);
            let refused = std::panic::catch_unwind(|| {
                let (literals, mask) = literals_and_code_mask(&source);
                let sql = literals
                    .iter()
                    .find(|value| value.text.starts_with("SELECT"))
                    .expect("SELECT fixture");
                composed_library_path_candidate(&source, sql, &literals, &mask)
            });
            assert!(
                refused.is_err(),
                "unsupported helper expression must fail closed: {argument}"
            );
        }
    }

    #[test]
    fn library_path_constructor_refuses_explicit_format_arguments_and_aliases() {
        for source in [
            r#"fn query() { let paths_are = super::library_paths_are_sql("$1", "$2"); let sql = format!("SELECT $1 WHERE {paths_are}", paths_are = "$4 = $4"); }"#,
            r#"fn query() { let sql = format!("SELECT {0}", super::library_paths_are_sql("$1", "$2")); }"#,
            r#"fn query() { let sql = format!("SELECT {p}", p = super::library_paths_are_sql("$1", "$2")); }"#,
            r#"fn query() { let paths_are = super::library_paths_are_sql("$1", "$2"); let alias = paths_are; let sql = format!("SELECT {alias}"); }"#,
            r#"fn query() { let paths = format!("{}", super::library_paths_are_sql("$1", "$4")); let sql = format!("SELECT $1 WHERE {paths} AND $2 = $2"); }"#,
        ] {
            let refused = std::panic::catch_unwind(|| {
                let (literals, mask) = literals_and_code_mask(source);
                let sql = literals
                    .iter()
                    .find(|value| value.text.starts_with("SELECT"))
                    .expect("SELECT fixture");
                composed_library_path_candidate(source, sql, &literals, &mask)
            });
            assert!(
                refused.is_err(),
                "unsupported format authority must fail closed: {source}"
            );
        }
    }

    #[test]
    fn composed_path_statement_does_not_become_a_later_direct_binding() {
        let source = r#"fn query() {
            let sql = format!("INSERT INTO roots SELECT $1, $2 WHERE $3 AND {}", super::library_paths_are_sql("$1", "$4"));
            conn.execute(sql, params![library, fingerprint, establish, paths]);
            let rows = conn.query("SELECT fingerprint FROM roots WHERE library_id = $1");
        }"#;
        let (literals, mask) = literals_and_code_mask(source);
        for sql in literals.iter().filter(|literal| {
            literal.text.starts_with("INSERT") || literal.text.starts_with("SELECT")
        }) {
            let variants =
                principal_templates("hiqlite_media.rs", source, sql, &literals, &mask, &[]);
            assert_eq!(variants.len(), 1);
            validate_sql(&variants[0])
                .expect("both independently composed statements retain their own parameters");
        }
    }

    #[test]
    fn named_path_predicate_does_not_own_unrelated_nested_lease_sql() {
        let source = r#"fn query() {
            let paths_are = super::library_paths_are_sql("$1", "$2");
            let sql = format!("INSERT INTO guards SELECT $1 WHERE {paths_are}");
            if let Some(lease) = lease {
                let sql = format!("SELECT id FROM jobs WHERE lease = $1");
            }
        }"#;
        let (literals, mask) = literals_and_code_mask(source);
        for sql in literals
            .iter()
            .filter(|literal| is_sql_shaped(&literal.text))
        {
            let variants =
                principal_templates("hiqlite_media.rs", source, sql, &literals, &mask, &[]);
            assert_eq!(variants.len(), 1);
            validate_sql(&variants[0]).expect("each SQL statement keeps its own binding scope");
        }
    }

    #[test]
    fn completed_sqlite_path_query_does_not_own_later_mutation_or_read() {
        let source = r#"fn query() {
            let current: bool = conn.query_row(
                &format!("SELECT {}", crate::store::library_paths_are_sql("?1", "?2")),
                params![library, paths], |row| row.get(0),
            )?;
            let deleted = conn.execute("DELETE FROM files WHERE id = ?1", params![file])?;
            let inserted = conn.execute("INSERT INTO roots VALUES (?1, ?2)", params![library, fingerprint])?;
            let rows = conn.query_row("SELECT fingerprint FROM roots WHERE id = ?1", params![library], |row| row.get(0))?;
        }"#;
        let (literals, mask) = literals_and_code_mask(source);
        for sql in literals
            .iter()
            .filter(|literal| is_sql_shaped(&literal.text))
        {
            let variants =
                principal_templates("publication.rs", source, sql, &literals, &mask, &[]);
            assert_eq!(variants.len(), 1);
            let maximum =
                validate_sqlite_placeholders(&variants[0]).expect("sequential SQLite parameters");
            assert_eq!(
                same_statement_binding_arity(source, &mask, sql.end),
                Some(maximum)
            );
        }
    }

    #[test]
    fn completed_atomic_publication_does_not_own_later_path_reads() {
        let source = r#"fn query() {
            let results = self.atomic_publication(lease, replacement, vec![
                (format!("INSERT INTO roots SELECT $1, $2 WHERE $3 AND {}",
                    crate::store::library_paths_are_sql("$1", "$4")),
                    params!(library, fingerprint, establish, paths)),
                ("UPDATE roots SET fingerprint = fingerprint WHERE library_id = $1".to_owned(),
                    params!(library)),
            ]).await?;
            let rows = self.client().query("SELECT fingerprint FROM roots WHERE library_id = $1");
        }"#;
        let (literals, mask) = literals_and_code_mask(source);
        for sql in literals.iter().filter(|sql| is_sql_shaped(&sql.text)) {
            let variants =
                principal_templates("hiqlite_publication.rs", source, sql, &literals, &mask, &[]);
            assert_eq!(variants.len(), 1);
            validate_sql(&variants[0])
                .expect("each publication statement keeps its own parameters");
            if sql.text.starts_with("INSERT") {
                assert!(
                    variants[0].contains("$4"),
                    "composed path-set placeholder stays required"
                );
            }
        }
    }

    #[test]
    fn atomic_publication_completion_refuses_helpers_outside_modeled_sql() {
        for expression in [
            r#"self.atomic_publication(crate::store::library_paths_are_sql("$1", "$2"), replacement, vec![("SELECT $1".to_owned(), params!(library))]).await?"#,
            r#"self.atomic_publication(lease, replacement, vec![("SELECT $1".to_owned(), params!(crate::store::library_paths_are_sql("$1", "$2")))]).await?"#,
            r#"self.atomic_publication(lease, replacement, vec![(format!("not SQL {}", crate::store::library_paths_are_sql("$1", "$2")), params!(library, paths))]).await?"#,
            r#"other.atomic_publication(lease, replacement, vec![(format!("SELECT {}", crate::store::library_paths_are_sql("$1", "$2")), params!(library, paths))]).await?"#,
            r#"self.atomic_publication(lease, replacement, vec![(wrapper(format!("SELECT {}", crate::store::library_paths_are_sql("$1", "$2"))), params!(library, paths))]).await?"#,
        ] {
            let source = format!(
                r#"fn query() {{ let results = {expression}; let rows = self.client().query("SELECT id FROM roots WHERE id = $1"); }}"#
            );
            assert!(
                std::panic::catch_unwind(|| {
                    let (literals, mask) = literals_and_code_mask(&source);
                    let later = literals
                        .iter()
                        .find(|sql| sql.text.starts_with("SELECT id"))
                        .expect("later read");
                    principal_templates(
                        "hiqlite_publication.rs",
                        &source,
                        later,
                        &literals,
                        &mask,
                        &[],
                    )
                })
                .is_err(),
                "unreviewed helper location stays refused: {expression}"
            );
        }
    }

    #[test]
    fn atomic_publication_completion_refuses_dynamic_path_placeholders() {
        let source = r#"fn query() {
            let results = self.atomic_publication(lease, replacement, vec![
                (format!("SELECT {}", crate::store::library_paths_are_sql(slot, "$2")), params!(library, paths)),
            ]).await?;
            let rows = self.client().query("SELECT id FROM roots WHERE id = $1");
        }"#;
        assert!(std::panic::catch_unwind(|| {
            let (literals, mask) = literals_and_code_mask(source);
            let later = literals
                .iter()
                .find(|sql| sql.text.starts_with("SELECT id"))
                .expect("later read");
            principal_templates(
                "hiqlite_publication.rs",
                source,
                later,
                &literals,
                &mask,
                &[],
            )
        })
        .is_err());
    }

    #[test]
    fn unrelated_sql_literal_cannot_admit_an_unmodeled_helper_wrapper() {
        for expression in [
            r#"wrapper("SELECT ?1", crate::store::library_paths_are_sql("?1", "?2"))"#,
            r#"conn.query_row(&format!("SELECT ?1", crate::store::library_paths_are_sql("?1", "?2")), params![library], |row| row.get(0))?"#,
            r#"conn.query_row(&format!("SELECT ?1"), params![crate::store::library_paths_are_sql("?1", "?2")], |row| row.get(0))?"#,
        ] {
            let source = format!(
                r#"fn query() {{ let prior = {expression}; let rows = conn.query_row("SELECT id FROM roots WHERE id = ?1", params![library], |row| row.get(0)); }}"#
            );
            let refused = std::panic::catch_unwind(|| {
                let (literals, mask) = literals_and_code_mask(&source);
                let sql = literals
                    .iter()
                    .find(|literal| literal.text.starts_with("SELECT id"))
                    .expect("later query");
                principal_templates("publication.rs", &source, sql, &literals, &mask, &[])
            });
            assert!(
                refused.is_err(),
                "unmodeled helper remains refused: {expression}"
            );
        }
    }

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

    #[test]
    fn root_removal_composition_preserves_placeholder_order() {
        let lease = hiqlite::params!("resource", "owner", 1_i64, 2_i64, 3_i64);
        for fence in [None, Some(&lease)] {
            let statements =
                super::super::hiqlite_media::root_removal_statements(7, "[]", "[]", fence)
                    .expect("actual root-removal composition validates its SQL");
            assert!(!statements.is_empty());
            for (sql, _) in statements {
                validate_sql(&sql)
                    .expect("every actual statement introduces placeholders in order");
                assert_eq!(sql.contains("FROM job_leases"), fence.is_some(), "{sql}");
                assert!(!sql.contains("{LEASE}"), "the actual fence is substituted");
            }
        }
    }

    #[test]
    fn library_paths_helper_preserves_placeholder_introduction_order() {
        for (helper, paths, valid) in [
            ("crate::store", "$4", true),
            ("crate::store", "$6", false),
            ("super", "$4", true),
            ("super", "$6", false),
        ] {
            let source = format!(
                r#"fn fenced() {{ let sql = format!("INSERT INTO library_roots SELECT $1, $2 WHERE $3 AND {{}} AND lease=$5", {helper}::library_paths_are_sql("$1", "{paths}")); }}"#
            );
            let (literals, mask) = literals_and_code_mask(&source);
            let sql = literals
                .iter()
                .find(|value| value.text.starts_with("INSERT"))
                .expect("SQL template");
            let variants = principal_templates(
                "hiqlite_publication.rs",
                &source,
                sql,
                &literals,
                &mask,
                &[],
            );
            assert_eq!(variants.len(), 1);
            assert!(
                variants[0].contains(&format!("json_each({paths})")),
                "actual helper expanded"
            );
            assert_eq!(validate_sql(&variants[0]).is_ok(), valid, "{}", variants[0]);
        }
    }

    #[test]
    fn principal_format_arguments_end_at_their_own_call_without_a_trailing_comma() {
        let source = r#"fn read() { let sql = format!("SELECT {} WHERE {} AND playback_id=$2", layout.projection(), layout.equals(1)); let later = layout.equals(9); }"#;
        let (literals, mask) = literals_and_code_mask(source);
        let sql = literals
            .iter()
            .find(|literal| literal.text.starts_with("SELECT"))
            .expect("SQL literal");
        assert_eq!(
            positional_format_arguments(source, &mask, sql.end)
                .iter()
                .map(|argument| argument.trim())
                .collect::<Vec<_>>(),
            ["layout.projection()", "layout.equals(1)"]
        );
        let variants =
            principal_templates("hiqlite_sessions.rs", source, sql, &literals, &mask, &[]);
        assert_eq!(variants.len(), 2);
        for variant in variants {
            validate_sql(&variant).expect("real principal predicate retains parameter one");
        }
    }

    #[test]
    fn local_owner_helper_is_not_misread_as_the_source_owner_closure() {
        let source = r#"fn read() { let owner_1 = local_owner_predicate(rebuilt, 1); let sql = format!("SELECT ?2 WHERE {owner_1}"); }"#;
        let (literals, mask) = literals_and_code_mask(source);
        let sql = literals
            .iter()
            .find(|literal| literal.text.starts_with("SELECT"))
            .expect("SQL literal");
        let variants = principal_templates("sessions.rs", source, sql, &literals, &mask, &[]);
        assert_eq!(variants.len(), 2);
        for variant in variants {
            assert_eq!(
                validate_sqlite_placeholders(&variant).expect("complete actual owner binding"),
                2
            );
        }
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

    #[test]
    fn sqlite_rejects_gaps_mixed_spelling_and_out_of_range_arity() {
        let gap = validate_sqlite_placeholders("SELECT ?1, ?2, ?4")
            .expect_err("an indexed gap is silent at runtime and must fail the census");
        assert!(gap.contains("missing ?3"), "the error names the gap: {gap}");

        let mixed = validate_sqlite_placeholders("SELECT ?, ?1")
            .expect_err("anonymous and indexed parameters cannot share a statement");
        assert!(mixed.contains("bare ? cannot be mixed"));

        let source = r#"conn.execute("SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13", params![
            one, two, three, four, five, six, seven, eight, nine, ten, eleven, twelve,
        ])"#;
        let (literals, is_code) = literals_and_code_mask(source);
        let literal = literals.first().expect("the SQL literal is scanned");
        assert_eq!(validate_sqlite_placeholders(&literal.text), Ok(13));
        assert_eq!(
            same_statement_binding_arity(source, &is_code, literal.end),
            Some(12),
            "a trailing macro comma is not a thirteenth binding"
        );
    }

    #[test]
    fn sqlite_ignores_placeholder_spelling_in_literals_comments_and_identifiers() {
        assert_eq!(
            validate_sqlite_placeholders(
                "SELECT '?9', \"?8\", `[?7]`, [?6] FROM t WHERE id = ?1 -- ?5\n/* ?4 */"
            ),
            Ok(1)
        );
    }
}
