//! The spawn census: every production child goes through
//! [`super::spawn_job_owned`], so every child has a priority class and a row
//! on the Activity page. Plan P-02 §3.2.1 found eleven production spawns that
//! did not; this test is what keeps the count at zero.
//!
//! It reads the source of every crate linked into the daemon, blanks
//! comments and literals, removes every `#[cfg(test)]` item and every file a
//! `#[cfg(test)] mod` pulls in, and then looks for the ways a process can be
//! started without the launcher: a zero-argument `.spawn()`, `.output()` or
//! `.status()` on a command, the same methods called as `Command::spawn(…)`,
//! a job attached by hand, and a raw `fork`/`vfork`/`posix_spawn`. The only
//! exemptions are the launcher's own `.spawn()` and `ChildJob::attach`.
//!
//! Text can only guess which receiver is a command. The typed half of the
//! census is clippy's `disallowed-methods` (`clippy.toml`):
//! `Command::{spawn, output, status}` for tokio and std are refused in every
//! crate's production build and allowed only under `cfg(test)`. The last
//! test here pins that configuration so neither half can be dropped quietly.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// The crates whose code runs inside plurxd. `plurx-cluster-check` is a
/// separate harness binary and never a daemon child.
const DAEMON_CRATES: [&str; 4] = ["plurx-core", "plurxd", "plurx-pgs", "plurx-compat-plex"];

/// Replace comments, string literals and character literals with spaces,
/// keeping every byte offset and newline, so braces and patterns inside them
/// cannot be mistaken for code.
pub(super) fn blank_non_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = source[i..].find('\n').map_or(bytes.len(), |at| i + at);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                let mut j = i + 2;
                while j < bytes.len() && depth > 0 {
                    if bytes[j] == b'/' && bytes.get(j + 1) == Some(&b'*') {
                        depth += 1;
                        j += 2;
                    } else if bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/') {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            b'r' | b'b' | b'c'
                if (i == 0 || !is_ident(bytes[i - 1]))
                    && raw_string_start(&bytes[i..]).is_some() =>
            {
                let (prefix, hashes) = raw_string_start(&bytes[i..]).expect("checked");
                let body = i + prefix;
                let closing = format!("\"{}", "#".repeat(hashes));
                let end = source[body..]
                    .find(&closing)
                    .map_or(bytes.len(), |at| body + at + closing.len());
                blank(&mut out, i, end);
                i = end;
            }
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(bytes.len());
                blank(&mut out, i, end);
                i = end;
            }
            b'\'' => {
                // A character literal, or a lifetime / label (left alone).
                let rest = &bytes[i + 1..];
                let literal_len = if rest.first() == Some(&b'\\') {
                    rest.iter()
                        .skip(1)
                        .position(|b| *b == b'\'')
                        .map(|at| at + 3)
                } else {
                    let width = source[i + 1..].chars().next().map_or(1, char::len_utf8);
                    (rest.get(width) == Some(&b'\'')).then_some(width + 2)
                };
                match literal_len {
                    Some(len) => {
                        blank(&mut out, i, i + len);
                        i += len;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).expect("blanking keeps UTF-8 boundaries")
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `r"`, `r#"`, `br"`, `cr##"`… → (prefix length through the quote, hashes).
fn raw_string_start(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    if matches!(bytes.first(), Some(b'b' | b'c')) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = bytes[i..].iter().take_while(|b| **b == b'#').count();
    i += hashes;
    (bytes.get(i) == Some(&b'"')).then_some((i + 1, hashes))
}

/// The production part of one file: blanked, with every `#[cfg(test)]` item
/// removed. Returns the text and the names of test-only child modules
/// declared as `mod name;` (with their `#[path]`, if any).
pub(super) fn production_code(source: &str) -> (String, Vec<(String, Option<String>)>) {
    let code = blank_non_code(source);
    let bytes = code.as_bytes();
    let mut out = code.clone().into_bytes();
    let mut test_modules = Vec::new();
    let mut search = 0;
    while let Some(found) = code[search..].find("#[cfg(") {
        let start = search + found;
        let Some(attribute_end) = matching(bytes, start + 1, b'[', b']') else {
            break;
        };
        let condition = &code[start..=attribute_end];
        let test_only = condition.contains("test") && !condition.contains("not(test");
        if !test_only {
            search = attribute_end + 1;
            continue;
        }
        // Skip this and any further attributes, remembering a `#[path]`.
        let mut item = attribute_end + 1;
        let mut path_attribute = None;
        loop {
            let next = item + code[item..].len() - code[item..].trim_start().len();
            if code[next..].starts_with("#[") {
                let end = matching(bytes, next + 1, b'[', b']').unwrap_or(bytes.len() - 1);
                if code[next..end]
                    .trim_start_matches("#[")
                    .trim_start()
                    .starts_with("path")
                {
                    // The path is a string literal: read it from the source.
                    path_attribute = source[next..end].split('"').nth(1).map(ToOwned::to_owned);
                }
                item = end + 1;
            } else {
                item = next;
                break;
            }
        }
        let end = item_end(&code, item);
        let declaration = code[item..=end].trim();
        if declaration.ends_with(';') {
            let words = declaration
                .trim_end_matches(';')
                .split_whitespace()
                .collect::<Vec<_>>();
            if let Some(at) = words.iter().position(|word| *word == "mod") {
                if let Some(name) = words.get(at + 1) {
                    test_modules.push(((*name).to_owned(), path_attribute));
                }
            }
        }
        for byte in &mut out[start..=end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        search = end + 1;
    }
    (
        String::from_utf8(out).expect("blanking keeps UTF-8 boundaries"),
        test_modules,
    )
}

/// Where the item or expression starting at `item` ends (inclusive).
///
/// An item (`fn`, `mod`, `impl`, `struct`, `use`, …) ends at its first
/// top-level `;` or at the brace closing its first top-level block. Anything
/// else — a struct field, a struct-literal field, a `let` — ends at its
/// first top-level `,` or `;`, or just before the brace that closes its
/// parent. Removing too little only costs a false alarm; removing too much
/// would hide production code, so both rules stop early rather than late.
fn item_end(code: &str, item: usize) -> usize {
    let bytes = code.as_bytes();
    let rest = code[item..].trim_start();
    let rest = match rest.strip_prefix("pub") {
        Some(after) if after.starts_with('(') => after
            .find(')')
            .map_or(after, |close| &after[close + 1..])
            .trim_start(),
        Some(after) if after.starts_with(char::is_whitespace) => after.trim_start(),
        _ => rest,
    };
    let first_word = rest
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '!'))
        .next()
        .unwrap_or_default();
    let is_item = matches!(
        first_word,
        "fn" | "async"
            | "unsafe"
            | "const"
            | "static"
            | "mod"
            | "impl"
            | "struct"
            | "enum"
            | "trait"
            | "type"
            | "use"
            | "extern"
            | "union"
            | "macro_rules!"
    );
    let mut depth = 0usize;
    for (offset, byte) in bytes[item..].iter().enumerate() {
        let at = item + offset;
        match byte {
            b'(' | b'[' => depth += 1,
            b'{' if is_item && depth == 0 => {
                return matching(bytes, at, b'{', b'}').unwrap_or(bytes.len() - 1);
            }
            b'{' => depth += 1,
            b')' | b']' | b'}' => match depth.checked_sub(1) {
                Some(outer) => depth = outer,
                None => return at.saturating_sub(1),
            },
            b';' if depth == 0 => return at,
            b',' if depth == 0 && !is_item => return at,
            _ => {}
        }
    }
    bytes.len() - 1
}

fn matching(bytes: &[u8], open_at: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in bytes[open_at..].iter().enumerate() {
        if *byte == open {
            depth += 1;
        } else if *byte == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open_at + offset);
            }
        }
    }
    None
}

const SPAWN: &str = "a zero-argument `.spawn()`";
const ATTACH: &str = "a job attached outside the launcher";

/// Every way to start a process that does not go through the launcher, as
/// (line, what) pairs, in one file's production code.
pub(super) fn bypasses(production: &str) -> Vec<(usize, &'static str)> {
    // Whitespace removed, with each kept byte's offset, so a call split over
    // lines (`command\n    .spawn()`) matches the same pattern.
    let (compact, offsets): (String, Vec<usize>) = production
        .char_indices()
        .filter(|(_, ch)| !ch.is_whitespace())
        .map(|(at, ch)| (ch, at))
        .unzip();
    let line_of = |compact_at: usize| production[..offsets[compact_at]].matches('\n').count() + 1;
    let mut found = Vec::new();
    for (pattern, what) in [
        (".spawn()", SPAWN),
        ("Command::spawn(", "`Command::spawn(…)`"),
        ("Command::output(", "`Command::output(…)`"),
        ("Command::status(", "`Command::status(…)`"),
        ("ChildJob::attach(", ATTACH),
        ("libc::fork(", "a raw `fork`"),
        ("libc::vfork(", "a raw `vfork`"),
        ("posix_spawn", "a raw `posix_spawn`"),
    ] {
        for (at, _) in compact.match_indices(pattern) {
            found.push((line_of(at), what));
        }
    }
    // `.output()` and `.status()` are common method names (a Live TV
    // session has an `output()`, a cluster store a `status()`), so these
    // count only when the receiver is a command: built by `Command::new` in
    // the same chain, or a binding the file declares as one.
    for (pattern, what) in [
        (".output()", "`.output()` on a command"),
        (".status()", "`.status()` on a command"),
    ] {
        for (at, _) in compact.match_indices(pattern) {
            if receiver_is_a_command(&compact, at) {
                found.push((line_of(at), what));
            }
        }
    }
    found.sort_unstable();
    found
}

/// Whether the method call at `dot` (in whitespace-free code) is made on a
/// process command.
fn receiver_is_a_command(compact: &str, dot: usize) -> bool {
    let bytes = compact.as_bytes();
    // Walk back over the receiver chain: identifiers, paths, field and
    // method accesses, `?`, and balanced argument lists.
    let mut start = dot;
    while start > 0 {
        let byte = bytes[start - 1];
        if byte == b')' || byte == b']' {
            let (open, close) = if byte == b')' {
                (b'(', b')')
            } else {
                (b'[', b']')
            };
            let mut depth = 0usize;
            let mut at = start;
            while at > 0 {
                at -= 1;
                if bytes[at] == close {
                    depth += 1;
                } else if bytes[at] == open {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            start = at;
        } else if is_ident(byte) || matches!(byte, b'.' | b':' | b'?') {
            start -= 1;
        } else {
            break;
        }
    }
    let receiver = &compact[start..dot];
    if receiver.contains("Command::new(") {
        return true;
    }
    let root = receiver
        .trim_start_matches(':')
        .split(|ch: char| !is_ident(ch as u8))
        .next()
        .unwrap_or_default();
    !root.is_empty() && binding_is_a_command(compact, root)
}

/// Whether the file binds `name` to a command: `let [mut] name = …Command::new(…`
/// or a parameter/field `name: [&mut|&|mut] …Command`.
fn binding_is_a_command(compact: &str, name: &str) -> bool {
    let bytes = compact.as_bytes();
    for (at, _) in compact.match_indices(name) {
        let prefix = &compact[..at];
        let after = &compact[at + name.len()..];
        // Whitespace is gone, so `let mut command` reads `letmutcommand`.
        let by_let = prefix.ends_with("let") || prefix.ends_with("letmut");
        let word_start = at == 0 || !is_ident(bytes[at - 1]) || prefix.ends_with("mut");
        if !(by_let || word_start) || after.starts_with("::") {
            continue;
        }
        if let Some(value) = after.strip_prefix('=').filter(|_| by_let) {
            let statement = value.split(';').next().unwrap_or_default();
            if statement.contains("Command::new(") {
                return true;
            }
        } else if let Some(ty) = after.strip_prefix(':') {
            let ty = ty
                .strip_prefix("&mut")
                .or_else(|| ty.strip_prefix('&'))
                .unwrap_or(ty);
            let path_end = ty
                .find(|ch: char| !(is_ident(ch as u8) || ch == ':'))
                .unwrap_or(ty.len());
            let path = &ty[..path_end];
            if path == "Command" || path.ends_with("::Command") {
                return true;
            }
        }
    }
    false
}

fn walk(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = std::fs::read_dir(directory)
        .expect("read source directory")
        .map(|entry| entry.expect("directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Every `mod name;` a file declares, with its `#[path]`, test-only or not.
/// Used for files that are themselves test-only: everything they pull in is.
fn module_declarations(source: &str) -> Vec<(String, Option<String>)> {
    let code = blank_non_code(source);
    let mut found = Vec::new();
    for (at, _) in code.match_indices("mod ") {
        if at > 0 && is_ident(code.as_bytes()[at - 1]) {
            continue;
        }
        let rest = code[at + 4..].trim_start();
        let name = rest
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect::<String>();
        if name.is_empty() || !rest[name.len()..].trim_start().starts_with(';') {
            continue;
        }
        let attributes = code[..at].rfind([';', '}', '{']).map_or(0, |end| end + 1);
        let path = source[attributes..at]
            .find("#[path")
            .and_then(|path| source[attributes + path..at].split('"').nth(1))
            .map(ToOwned::to_owned);
        found.push((name, path));
    }
    found
}

/// `path` with its `.` and `..` components resolved lexically, so a file
/// named as `tests/../../x.rs` compares equal to the `x.rs` the walk found.
fn lexical(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The files a source pulls in with `include!("…")`.
fn included_files(source: &str) -> Vec<String> {
    let code = blank_non_code(source);
    code.match_indices("include!(")
        .filter_map(|(at, _)| source[at..].split('"').nth(1).map(ToOwned::to_owned))
        .collect()
}

/// Every production source file of the daemon's crates with its production
/// code: test-only modules, and everything they declare or include, removed.
pub(super) fn daemon_production_sources() -> Vec<(PathBuf, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .to_owned();
    let mut sources = Vec::new();
    for name in DAEMON_CRATES {
        let root = crates.join(name).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        let texts = files
            .iter()
            .map(|file| std::fs::read_to_string(file).expect("read source"))
            .collect::<Vec<_>>();
        let mut excluded = BTreeSet::<PathBuf>::new();
        let is_excluded = |excluded: &BTreeSet<PathBuf>, file: &Path| {
            excluded.iter().any(|path| file.starts_with(path))
        };
        // A test-only file can be declared by a file earlier or later in the
        // walk, so resolve to a fixed point.
        loop {
            let mut changed = false;
            for (file, source) in files.iter().zip(&texts) {
                let parent = file.parent().expect("parent");
                let module_dir = match file.file_stem().and_then(|stem| stem.to_str()) {
                    Some("mod" | "lib" | "main") => parent.to_owned(),
                    Some(stem) => parent.join(stem),
                    None => continue,
                };
                let test_only = is_excluded(&excluded, file);
                let children = if test_only {
                    for include in included_files(source) {
                        changed |= excluded.insert(lexical(parent.join(include)));
                    }
                    module_declarations(source)
                } else {
                    production_code(source).1
                };
                for (name, path) in children {
                    let targets = match path {
                        Some(path) => vec![parent.join(path)],
                        None => vec![
                            module_dir.join(format!("{name}.rs")),
                            module_dir.join(&name),
                        ],
                    };
                    for target in targets {
                        changed |= excluded.insert(lexical(target));
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for (file, source) in files.into_iter().zip(texts) {
            if !is_excluded(&excluded, &file) {
                sources.push((file, production_code(&source).0));
            }
        }
    }
    sources
}

#[test]
fn every_production_spawn_goes_through_the_launcher() {
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/process/mod.rs");
    let mut violations = Vec::new();
    let mut launcher_spawns = 0;
    let mut files = 0;
    for (file, production) in daemon_production_sources() {
        files += 1;
        for (line, what) in bypasses(&production) {
            if file == launcher && what == SPAWN {
                launcher_spawns += 1;
                continue;
            }
            if file == launcher && what == ATTACH {
                continue;
            }
            violations.push(format!("{}:{line}: {what}", file.display()));
        }
    }
    assert!(files > 100, "the census read only {files} files");
    assert_eq!(
        launcher_spawns, 2,
        "spawn_job_owned and output_job_owned_blocking are the production `.spawn()`s"
    );
    assert!(
        violations.is_empty(),
        "production children must be started by process::spawn_job_owned (or \
         output_job_owned / status_job_owned / bounded::output), which gives \
         them a priority class and an Activity row:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_census_sees_through_formatting_literals_and_test_items() {
    let source = r##"
fn production() {
    let _ = tokio::process::Command::new("a")
        .arg("x")
        .spawn();
    let text = "command.spawn()";
    // command.output()
    let _ = r#"child.output()"#;
    let _ = '{';
}

#[cfg(test)]
mod tests {
    fn helper() { std::process::Command::new("b").status(); }
}

#[cfg(test)]
fn only_in_tests() { cmd.output(); }

#[cfg(test)]
#[path = "fixtures_tests.rs"]
mod fixtures;

fn after() {
    let _ = std::process::Command::new("c");
    let status = tokio::process::Command::output(&mut command).await;
}

struct Owner {
    child: Child,
    #[cfg(test)]
    reaped: Option<Sender<()>>,
}

impl Owner {
    fn start(command: &mut Command) { command.spawn(); }
    fn height(session: &Session) -> u32 { session.output().height }
    fn run(probe: &mut tokio::process::Command) { let _ = timeout(d, probe.output()); }
    fn store(cluster: &Store) { let _ = cluster.status().await; }
}
"##;
    let (production, test_modules) = production_code(source);
    assert_eq!(
        test_modules,
        vec![("fixtures".to_owned(), Some("fixtures_tests.rs".to_owned()))]
    );
    let found = bypasses(&production);
    assert_eq!(
        found,
        vec![
            (5, SPAWN),
            (26, "`Command::output(…)`"),
            (36, SPAWN),
            (38, "`.output()` on a command"),
        ],
        "{production}"
    );
}

/// `vod/tests/chunk_01.rs` pulls `vodencode_tests.rs` in as
/// `include!("../../vodencode_tests.rs")`; the excluded path must compare
/// equal to the file the walk found, or the census reads a test file as
/// production code.
#[test]
fn an_include_through_parent_directories_names_the_walked_file() {
    let root = Path::new("/w/crates/plurxd/src");
    let included = lexical(root.join("vod/tests").join("../../vodencode_tests.rs"));
    assert_eq!(included, root.join("vodencode_tests.rs"));
    assert!(root.join("vodencode_tests.rs").starts_with(&included));
    assert_eq!(lexical(root.join("./a/./b.rs")), root.join("a/b.rs"));
}

/// The typed half of the census: clippy refuses the six spawning methods in
/// production code, and each daemon crate allows them only in its tests.
#[test]
fn clippy_refuses_every_spawning_method_outside_tests() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let config = std::fs::read_to_string(workspace.join("clippy.toml")).expect("clippy.toml");
    for library in ["tokio", "std"] {
        for method in ["spawn", "output", "status"] {
            let path = format!("\"{library}::process::Command::{method}\"");
            assert!(config.contains(&path), "clippy.toml must disallow {path}");
        }
    }
    let crates = workspace.join("crates");
    for (root, allow) in [
        (
            "plurx-core/src/lib.rs",
            "#![cfg_attr(test, allow(clippy::disallowed_methods))]",
        ),
        (
            "plurxd/src/main.rs",
            "#![cfg_attr(test, allow(clippy::disallowed_methods))]",
        ),
    ] {
        let source = std::fs::read_to_string(crates.join(root)).expect("crate root");
        assert!(
            source.contains(allow),
            "{root} allows the spawning methods in tests only: {allow}"
        );
        assert!(
            !source.contains("#![allow(clippy::disallowed_methods)]"),
            "{root} must not allow the spawning methods in production"
        );
    }
}
