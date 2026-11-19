//! A source census of the membership manager's replicated calls.
//!
//! Scope: the membership manager only — `cluster/membership.rs` (up to its
//! test module) and the files directly in `cluster/membership/`, not
//! recursively, skipping `*tests.rs`. The rest of `cluster/` (migration,
//! coordination, clock) is not censused here.
//!
//! The membership manager does not go through the Store's `TimedClient`; it
//! issues its SQL and quorum watermarks through
//! [`crate::store::CountedClient`], which counts each call by class and by a
//! `&'static str` label into `plurx_store_operations_total` and the
//! by-method families. The type does the enforcing: the raw hiqlite client is
//! a private field, so an unlabelled call does not compile. This census keeps
//! the rest of the contract from regressing silently:
//!
//! - every replicated call in the manager's production source — any SQL
//!   method of hiqlite's `Client` (the vocabulary is read from the vendored
//!   client itself), `db_quorum_watermark`, or a local helper whose first
//!   parameter is `label: &'static str` — passes a label first, and the label
//!   is the name of the function that issues it, or that function's own
//!   forwarded `label` ([`failures`]). The labels share one namespace with
//!   the Store's method labels in the HTTP and background families, so a
//!   function whose name is also a function name in `store/` is labelled
//!   `membership_<name>` instead, and only then. It is never a `file:line` and never
//!   built at run time, so the metric's label set is bounded by the source;
//! - no production function takes a raw `hiqlite::Client` except the
//!   constructors that wrap the one they are handed, and the public entry
//!   points integration tests call with a raw client, which must wrap theirs;
//! - `CountedClient` hands out no raw client and derefs to nothing.
//!
//! It is a text census, like `store::consistent_read_census`: comments and
//! string contents are blanked before it looks, and it does not expand
//! macros, so it sees calls inside them as written.

use std::collections::BTreeSet;

/// The manager's production sources. [`the_census_covers_every_membership_source`]
/// keeps this list complete.
const SOURCES: &[(&str, &str)] = &[
    ("membership.rs", include_str!("../membership.rs")),
    ("membership/directory.rs", include_str!("directory.rs")),
    ("membership/lifecycle.rs", include_str!("lifecycle.rs")),
    (
        "membership/sharing_source_schema.rs",
        include_str!("sharing_source_schema.rs"),
    ),
];

/// The constructors that receive the node's raw client and wrap it in
/// `FlooredClient`.
const RAW_CLIENT_CONSTRUCTORS: &[&str] =
    &["replicated", "clock_observation", "construct_replicated"];

/// Functions that may take a raw `hiqlite::Client`: the constructors, and the
/// public entry points the store contract tests call with a fixture's raw
/// client, which must wrap it with `CountedClient::membership` (checked).
const RAW_CLIENT_FUNCTIONS: &[&str] = &[
    "replicated",
    "clock_observation",
    "construct_replicated",
    "sharing_session_principal_floor_ready",
    "sharing_catalogue_item_identity_floor_ready",
    "sharing_member_floor_ready",
    "bootstrap_purpose_admission_schema_for_contract",
    "observe_purpose_key_members_for_contract",
    "observe_source_admission_members_for_contract",
];

/// Calls that are replicated operations but are not SQL methods of the
/// hiqlite client.
const NON_SQL_REPLICATED: &[&str] = &["db_quorum_watermark"];

/// Fewer labelled sites than this means the scanner stopped seeing them
/// (there are about 140), not that the manager stopped making calls.
const MIN_LABELLED_SITES: usize = 130;

fn production(name: &str, source: &str) -> String {
    if name == "membership.rs" {
        source
            .split_once("\n#[cfg(test)]\nmod tests;")
            .expect("membership.rs declares its test module")
            .0
            .to_owned()
    } else {
        source.to_owned()
    }
}

/// `source` with comments and the contents of string and char literals
/// replaced by spaces (newlines kept), so offsets still line up. String
/// delimiters stay, so a literal argument is still recognisable.
fn code_mask(source: &str) -> Vec<u8> {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to.min(bytes.len())] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut index = 0;
    while index < bytes.len() {
        let rest = &source[index..];
        let after_ident = index > 0 && {
            let previous = bytes[index - 1];
            previous.is_ascii_alphanumeric() || previous == b'_'
        };
        if rest.starts_with("//") {
            let end = rest.find('\n').map_or(bytes.len(), |offset| index + offset);
            blank(&mut out, index, end);
            index = end;
        } else if rest.starts_with("/*") {
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
            blank(&mut out, index, cursor);
            index = cursor;
        } else if !after_ident && (rest.starts_with("r#") || rest.starts_with("r\"")) {
            let hashes = rest[1..].bytes().take_while(|byte| *byte == b'#').count();
            let open = index + 1 + hashes;
            if bytes.get(open) != Some(&b'"') {
                index += 1;
                continue;
            }
            let terminator = format!("\"{}", "#".repeat(hashes));
            let end = source[open + 1..]
                .find(&terminator)
                .map_or(bytes.len(), |offset| open + 1 + offset);
            blank(&mut out, open + 1, end);
            index = end + terminator.len();
        } else if bytes[index] == b'"' {
            let mut cursor = index + 1;
            while cursor < bytes.len() && bytes[cursor] != b'"' {
                cursor += if bytes[cursor] == b'\\' { 2 } else { 1 };
            }
            blank(&mut out, index + 1, cursor);
            index = cursor + 1;
        } else if bytes[index] == b'\'' {
            // A char literal, or a lifetime; only the former is blanked.
            let body = &rest[1..];
            let width = if body.starts_with('\\') {
                body.get(2..)
                    .and_then(|escaped| escaped.find('\''))
                    .map(|offset| offset + 2)
                    .filter(|width| *width <= 12)
            } else {
                body.chars()
                    .next()
                    .map(char::len_utf8)
                    .filter(|width| body.as_bytes().get(*width) == Some(&b'\''))
            };
            match width {
                Some(width) => {
                    blank(&mut out, index + 1, index + 1 + width);
                    index += width + 2;
                }
                None => index += 1,
            }
        } else {
            index += 1;
        }
    }
    out
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn ident_at(mask: &[u8], at: usize) -> &str {
    let end = (at..mask.len())
        .find(|index| !is_ident(mask[*index]))
        .unwrap_or(mask.len());
    std::str::from_utf8(&mask[at..end]).expect("ascii identifier")
}

fn skip_space(mask: &[u8], mut at: usize) -> usize {
    while at < mask.len() && mask[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

/// Every `fn name` with a body: `(name, signature start, body start, body end)`.
fn functions(mask: &[u8]) -> Vec<(String, usize, usize, usize)> {
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(offset) = find(mask, index, b"fn ") {
        let at = offset;
        index = at + 3;
        if at > 0 && is_ident(mask[at - 1]) {
            continue;
        }
        let name_at = skip_space(mask, at + 3);
        let name = ident_at(mask, name_at).to_owned();
        if name.is_empty() {
            continue;
        }
        // The body is the first `{` at bracket depth zero; a `;` first means
        // a declaration without one.
        let mut depth = 0_i32;
        let mut cursor = name_at + name.len();
        let mut body = None;
        while cursor < mask.len() {
            match mask[cursor] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b';' if depth == 0 => break,
                b'{' if depth == 0 => {
                    body = Some(cursor);
                    break;
                }
                _ => {}
            }
            cursor += 1;
        }
        let Some(body) = body else { continue };
        let mut depth = 0_i32;
        let mut end = body;
        while end < mask.len() {
            match mask[end] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            end += 1;
        }
        found.push((name, name_at, body, end));
    }
    found
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn enclosing(functions: &[(String, usize, usize, usize)], at: usize) -> Option<&str> {
    functions
        .iter()
        .filter(|(_, _, body, end)| *body <= at && at <= *end)
        .max_by_key(|(_, _, body, _)| *body)
        .map(|(name, ..)| name.as_str())
}

fn line_of(source: &str, at: usize) -> usize {
    source[..at].matches('\n').count() + 1
}

/// hiqlite's SQL methods, read from the vendored client so a method added
/// there is in the vocabulary without anyone remembering to add it here.
/// Every function name declared anywhere under `src/store/`, tests
/// included: the names a Store-side label can take.
fn store_functions() -> BTreeSet<String> {
    fn walk(directory: &std::path::Path, names: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(directory).expect("store directory") {
            let path = entry.expect("store entry").path();
            if path.is_dir() {
                walk(&path, names);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).expect("store source");
                let mask = code_mask(&text);
                let mut from = 0;
                while let Some(at) = find(&mask, from, b"fn ") {
                    from = at + 3;
                    if at > 0 && is_ident(mask[at - 1]) {
                        continue;
                    }
                    let name = ident_at(&mask, skip_space(&mask, at + 3));
                    if !name.is_empty() {
                        names.insert(name.to_owned());
                    }
                }
            }
        }
    }
    let mut names = BTreeSet::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store"),
        &mut names,
    );
    assert!(names.contains("get_settings"), "the store scan is blind");
    names
}

fn hiqlite_sql_methods() -> BTreeSet<String> {
    let client =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/hiqlite/src/client");
    let mut methods = BTreeSet::new();
    for file in ["query.rs", "execute.rs", "transaction.rs", "batch.rs"] {
        let text = std::fs::read_to_string(client.join(file)).expect("vendored hiqlite client");
        for line in text.lines() {
            let line = line.trim_start();
            let Some(rest) = line
                .strip_prefix("pub async fn ")
                .or_else(|| line.strip_prefix("pub fn "))
            else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            methods.insert(name);
        }
    }
    assert!(
        [
            "query_consistent_map",
            "query_map",
            "execute",
            "txn",
            "batch"
        ]
        .iter()
        .all(|name| methods.contains(*name)),
        "the vocabulary must hold hiqlite's SQL methods: {methods:?}"
    );
    methods
}

/// Everything the census finds wrong in `sources`, one line each; and the
/// number of labelled sites it checked.
fn failures(
    sources: &[(&str, String)],
    vocabulary: &BTreeSet<String>,
    store_functions: &BTreeSet<String>,
) -> (Vec<String>, usize) {
    let mut problems = Vec::new();
    let mut sites = 0;
    let masks: Vec<Vec<u8>> = sources.iter().map(|(_, text)| code_mask(text)).collect();
    // Helpers that take a label and forward it: any function, in any of the
    // sources, whose first parameter (after `&self`) is the label.
    let mut helpers = BTreeSet::new();
    for mask in &masks {
        for (name, name_at, body, _) in functions(mask) {
            let signature = String::from_utf8_lossy(&mask[name_at..body]).to_string();
            let parameters = signature
                .split_once('(')
                .map(|(_, rest)| rest.trim_start())
                .unwrap_or_default();
            let parameters = parameters
                .strip_prefix("&self,")
                .map_or(parameters, str::trim_start);
            if parameters.starts_with("label: &'static str") {
                helpers.insert(name);
            }
        }
    }
    for ((path, text), mask) in sources.iter().zip(&masks) {
        let functions = functions(mask);
        let mut at = 0;
        while at < mask.len() {
            if !is_ident(mask[at]) || (at > 0 && is_ident(mask[at - 1])) {
                at += 1;
                continue;
            }
            let name = ident_at(mask, at);
            let start = at;
            at += name.len().max(1);
            let before = (0..start)
                .rev()
                .find(|index| !mask[*index].is_ascii_whitespace())
                .map(|index| mask[index]);
            let method = before == Some(b'.');
            let counted = if method {
                vocabulary.contains(name)
                    || NON_SQL_REPLICATED.contains(&name)
                    || helpers.contains(name)
            } else {
                helpers.contains(name)
            };
            if !counted {
                continue;
            }
            // `fn name(` is the definition, not a call.
            if !method && start >= 3 && &mask[start - 3..start] == b"fn " {
                continue;
            }
            let mut cursor = skip_space(mask, start + name.len());
            if mask[cursor..].starts_with(b"::") {
                // Turbofish: skip the generic arguments.
                let mut depth = 0_i32;
                while cursor < mask.len() {
                    match mask[cursor] {
                        b'<' => depth += 1,
                        b'>' => {
                            depth -= 1;
                            if depth == 0 {
                                cursor += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
                cursor = skip_space(mask, cursor);
            }
            if mask.get(cursor) != Some(&b'(') {
                continue;
            }
            let caller = enclosing(&functions, start).unwrap_or("<module>");
            let line = line_of(text, start);
            let argument = skip_space(mask, cursor + 1);
            let label = if mask.get(argument) == Some(&b'"') {
                let close = argument
                    + 1
                    + mask[argument + 1..]
                        .iter()
                        .position(|byte| *byte == b'"')
                        .expect("closed literal");
                Some(text[argument + 1..close].to_owned())
            } else {
                None
            };
            sites += 1;
            match label {
                Some(label) if label == caller && !store_functions.contains(caller) => {}
                Some(label)
                    if label.strip_prefix("membership_") == Some(caller)
                        && store_functions.contains(caller) => {}
                Some(label) if label == caller => problems.push(format!(
                    "{path}:{line}: `{name}` in `{caller}` is labelled \"{label}\", which is also a function in store/; label it \"membership_{caller}\""
                )),
                Some(label) => problems.push(format!(
                    "{path}:{line}: `{name}` in `{caller}` is labelled \"{label}\"; the label is the issuing function's name"
                )),
                None if ident_at(mask, argument) == "label" && helpers.contains(caller) => {}
                None => problems.push(format!(
                    "{path}:{line}: `{name}` in `{caller}` is not labelled: its first argument must be \"{caller}\""
                )),
            }
        }
        // No raw hiqlite client outside the functions that wrap one.
        for needle in [
            ": &Client",
            ": &'a Client",
            ": Client",
            ": hiqlite::Client",
            ": &hiqlite::Client",
        ] {
            let mut from = 0;
            while let Some(found) = find(mask, from, needle.as_bytes()) {
                from = found + needle.len();
                if mask.get(from).copied().is_some_and(is_ident) {
                    continue;
                }
                let owner = enclosing(&functions, found).or_else(|| {
                    functions
                        .iter()
                        .find(|(_, name_at, body, _)| *name_at <= found && found < *body)
                        .map(|(name, ..)| name.as_str())
                });
                let line = line_of(text, found);
                match owner {
                    Some(owner) if RAW_CLIENT_FUNCTIONS.contains(&owner) => {
                        if !RAW_CLIENT_CONSTRUCTORS.contains(&owner) {
                            let (_, _, body, end) = functions
                                .iter()
                                .find(|(name, ..)| name == owner)
                                .expect("owner");
                            if !text[*body..*end].contains("CountedClient::membership(client.clone())") {
                                problems.push(format!(
                                    "{path}:{line}: `{owner}` must wrap its raw client in CountedClient::membership"
                                ));
                            }
                        }
                    }
                    owner => problems.push(format!(
                        "{path}:{line}: a raw hiqlite Client in `{}` reaches no counter; take `&CountedClient`",
                        owner.unwrap_or("<field>")
                    )),
                }
            }
        }
    }
    (problems, sites)
}

fn sources() -> Vec<(&'static str, String)> {
    SOURCES
        .iter()
        .map(|(name, text)| (*name, production(name, text)))
        .collect()
}

#[test]
fn every_membership_replicated_call_is_labelled_with_its_function() {
    let (problems, sites) = failures(&sources(), &hiqlite_sql_methods(), &store_functions());
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(
        sites >= MIN_LABELLED_SITES,
        "the census saw only {sites} labelled sites; the scanner is blind, not the manager quiet"
    );
}

#[test]
fn the_census_covers_every_membership_source() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cluster/membership");
    let mut present: Vec<String> = std::fs::read_dir(&directory)
        .expect("membership directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| {
            name.ends_with(".rs") && !name.ends_with("tests.rs") && name != "operation_census.rs"
        })
        .map(|name| format!("membership/{name}"))
        .collect();
    present.push("membership.rs".to_owned());
    present.sort();
    let mut censused: Vec<String> = SOURCES.iter().map(|(name, _)| (*name).to_owned()).collect();
    censused.sort();
    assert_eq!(
        present, censused,
        "a new membership source must join the census"
    );
}

/// The counted client is only a guarantee if it cannot hand the raw one out.
#[test]
fn the_counted_client_exposes_no_raw_client() {
    let source = include_str!("../../store/counted_client.rs");
    let code = String::from_utf8(code_mask(source)).expect("utf-8");
    let code = code
        .split_once("#[cfg(test)]\nmod tests")
        .map_or(code.as_str(), |(head, _)| head);
    for forbidden in [
        "Deref",
        "-> &Client",
        "-> Client",
        "-> &hiqlite::Client",
        "-> hiqlite::Client",
        "pub client",
        "pub(crate) client",
        "pub(super) client",
        "AsRef<Client>",
    ] {
        assert!(
            !code.contains(forbidden),
            "CountedClient must not expose `{forbidden}`"
        );
    }
}

/// Each rule rejects the implementation it exists to stop.
#[test]
fn the_census_rejects_unlabelled_mislabelled_and_raw_calls() {
    let vocabulary = hiqlite_sql_methods();
    let none = BTreeSet::new();
    let check = |source: &str| failures(&[("fixture.rs", source.to_owned())], &vocabulary, &none).0;
    assert!(check(
        "async fn refresh(c: &CountedClient) { c.query_map::<Row, _>(\"refresh\", \"SELECT 1\", params!()).await; }"
    )
    .is_empty());
    // A call with no label (the shape every site had before the counted client).
    assert_eq!(
        check("async fn refresh(c: &CountedClient) { c.query_consistent_map::<Row, _>(\"SELECT 1\", params!()).await; }").len(),
        1
    );
    // A label naming some other function.
    assert_eq!(
        check("async fn refresh(c: &CountedClient) { c\n    .execute(\n        \"heartbeat\",\n        sql,\n        params!(),\n    ).await; }").len(),
        1
    );
    // A label built at run time.
    assert_eq!(
        check("async fn refresh(c: &CountedClient) { c.txn(name, statements).await; }").len(),
        1
    );
    // The quorum watermark is a replicated read too.
    assert_eq!(
        check("async fn refresh(c: &CountedClient) { c.db_quorum_watermark().await; }").len(),
        1
    );
    // A labelled helper must be handed the caller's name, and may forward its own.
    let helper = "async fn predicate(label: &'static str, c: &CountedClient) { c.query_consistent_map::<Row, _>(label, g, p).await; }\n";
    assert!(check(&format!(
        "{helper}async fn verify(c: &CountedClient) {{ predicate(\"verify\", c).await; }}"
    ))
    .is_empty());
    assert_eq!(
        check(&format!(
            "{helper}async fn verify(c: &CountedClient) {{ predicate(c).await; }}"
        ))
        .len(),
        1
    );
    // Only a function that takes a label may forward one.
    assert_eq!(
        check("async fn verify(c: &CountedClient) { c.query_map::<Row, _>(label, g, p).await; }")
            .len(),
        1
    );
    // A raw client parameter, or a wrapper that forgets to wrap.
    assert_eq!(
        check("async fn verify(client: &Client) -> bool { true }").len(),
        1
    );
    assert_eq!(
        check("pub async fn observe_source_admission_members_for_contract(client: &Client) { observe(client).await }").len(),
        1
    );
    // A name the Store also uses is prefixed, and only such a name.
    let store = BTreeSet::from(["status".to_owned()]);
    let shared =
        |source: &str| failures(&[("fixture.rs", source.to_owned())], &vocabulary, &store).0;
    assert_eq!(
        shared(
            "async fn status(c: &CountedClient) { c.query_map::<Row, _>(\"status\", s, p).await; }"
        )
        .len(),
        1
    );
    assert!(shared(
        "async fn status(c: &CountedClient) { c.query_map::<Row, _>(\"membership_status\", s, p).await; }"
    )
    .is_empty());
    assert_eq!(
        check("async fn refresh(c: &CountedClient) { c.query_map::<Row, _>(\"membership_refresh\", s, p).await; }").len(),
        1
    );
    // A public raw-client entry point that forgets to wrap.
    assert_eq!(
        check("pub async fn sharing_member_floor_ready(client: &Client) -> bool { observe(client).await }").len(),
        1
    );
    // Comments and strings are not code.
    assert!(check(
        "async fn verify(c: &CountedClient) { // c.execute(sql, p)\n let s = \"c.txn(x)\"; }"
    )
    .is_empty());
}
