//! The settings call-site inventory of playback-start E1b
//! (`docs/streaming/PLAYBACK-START-E1-BUILD.md` §6.3), pinned over every
//! production file of this crate.
//!
//! A read *moves* to the node's applied view when its answer only chooses
//! how to serve or present a request; it *stays* a store (quorum) read when
//! it precedes a write or reservation in the same operation, sits under a
//! lock a settings save takes, decides authentication, trust, membership or
//! ownership, builds a save response, runs before the node serves, or runs in
//! a background worker (§12-6). The tests pin, by function: every read
//! through the view, every direct caller of a function that reads it, and
//! that no fence, ownership, worker or boot file reads it or calls one. A new
//! view read, one moved to another function, a new caller, or a renamed view
//! handle changes a pin: classify it against the rule, then update the pin
//! and say why in the PR. The caller check is one level deep; a chain through
//! two helpers is the reviewer's to follow.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Every function that reads the applied settings view, with the number of
/// reads in it. `true` marks a helper (its callers are pinned below); the
/// rest are route handlers.
const VIEW_READS: &[(&str, &str, usize, bool)] = &[
    // Polled pages and their helpers: the `settings` route group.
    ("http/developer.rs", "readiness", 1, false),
    ("http/system.rs", "configured_server_name", 1, true),
    ("http/system.rs", "features", 1, false),
    ("http/system.rs", "server_info", 3, false),
    // GET only: the PUT passes `SettingsDtoSource::Authority`.
    ("http/system.rs", "settings_dto", 1, true),
    ("http/system.rs", "system_info", 3, false),
    // Diagnostics only; `remote_placement_ready` admits, so it stays.
    ("media_pool.rs", "diagnostics", 2, true),
    // The playback request path: preference reads.
    ("http/chapter_thumbs.rs", "enabled", 1, true),
    (
        "http/hls/create.rs",
        "resolve_plan_for_principal_with_local_evidence",
        1,
        true,
    ),
    (
        "http/hls/link_receipts.rs",
        "network_priors_enabled",
        1,
        true,
    ),
    (
        "http/hls/link_receipts.rs",
        "processed_candidate_link_evidence",
        1,
        true,
    ),
    (
        "http/internal_media.rs",
        "subtitle_cluster_source_enabled",
        1,
        true,
    ),
    ("http/network.rs", "stored_prior", 1, true),
    ("http/preparation.rs", "status", 2, false),
    ("http/stream.rs", "decision_for_file", 2, true),
    ("http/stream.rs", "readrate_setting", 1, true),
    // D3b: the create's one planning read, for the ingress and a remote
    // owner (`planning`, and `planning_from_authority` twice for an owner).
    ("http/hls/start_plan.rs", "ingress_plan", 1, true),
    ("http/hls/start_plan.rs", "owner_plan", 3, true),
];

/// The direct callers of each helper above, `(helper file, helper,
/// caller file, caller)`.
const HELPER_CALLERS: &[(&str, &str, &str, &str)] = &[
    (
        "http/chapter_thumbs.rs",
        "enabled",
        "http/chapter_thumbs.rs",
        "refuse_when_off",
    ),
    (
        "http/chapter_thumbs.rs",
        "enabled",
        "http/developer.rs",
        "chapter_thumbnails",
    ),
    (
        "http/hls/create.rs",
        "resolve_plan_for_principal_with_local_evidence",
        "http/hls/create.rs",
        "resolve_plan_for_principal",
    ),
    (
        "http/hls/create.rs",
        "resolve_plan_for_principal_with_local_evidence",
        "http/hls/create.rs",
        "resolve_plan_with_scope",
    ),
    (
        "http/hls/link_receipts.rs",
        "network_priors_enabled",
        "http/hls/link_receipts.rs",
        "accept_negative_for_ack",
    ),
    (
        "http/hls/link_receipts.rs",
        "network_priors_enabled",
        "http/hls/link_receipts.rs",
        "filter_catalog",
    ),
    (
        "http/hls/link_receipts.rs",
        "network_priors_enabled",
        "http/hls/prepared_link.rs",
        "proposed_proof",
    ),
    (
        "http/hls/link_receipts.rs",
        "network_priors_enabled",
        "http/system.rs",
        "client_log",
    ),
    (
        "http/hls/link_receipts.rs",
        "processed_candidate_link_evidence",
        "http/hls/processed_upgrade.rs",
        "admit_processed_candidate",
    ),
    (
        "http/internal_media.rs",
        "subtitle_cluster_source_enabled",
        "http/internal_media.rs",
        "serve_subtitle_source",
    ),
    (
        "http/network.rs",
        "stored_prior",
        "http/hls/create.rs",
        "create_with_purpose_inner",
    ),
    (
        "http/network.rs",
        "stored_prior",
        "http/stream.rs",
        "decision",
    ),
    (
        "http/stream.rs",
        "decision_for_file",
        "http/stream.rs",
        "decision",
    ),
    (
        "http/stream.rs",
        "decision_for_file",
        "http/stream.rs",
        "decision_for_source_file",
    ),
    (
        "http/stream.rs",
        "readrate_setting",
        "http/stream.rs",
        "serve_stream_mp4",
    ),
    (
        "http/system.rs",
        "configured_server_name",
        "http/system.rs",
        "server_info",
    ),
    (
        "http/system.rs",
        "configured_server_name",
        "http/system.rs",
        "server_node",
    ),
    (
        "http/system.rs",
        "settings_dto",
        "http/system.rs",
        "get_settings",
    ),
    // With `SettingsDtoSource::Authority` (pinned below).
    (
        "http/system.rs",
        "settings_dto",
        "http/system.rs",
        "update_settings",
    ),
    (
        "media_pool.rs",
        "diagnostics",
        "http/internal_media.rs",
        "directory",
    ),
    (
        "http/hls/start_plan.rs",
        "ingress_plan",
        "http/hls/create.rs",
        "create_with_purpose_inner",
    ),
    (
        "http/hls/start_plan.rs",
        "owner_plan",
        "http/internal_media_sessions.rs",
        "start",
    ),
];

/// The one path from a stays file to a view read, by contract (D3b §4.3):
/// a remote owner's start plans once at the ingress's binding, and every
/// fence, ownership and admission read in that file stays a Store read.
/// `(helper file, helper, caller file, caller)`.
const STAYS_CHAINS: &[(&str, &str, &str, &str)] = &[(
    "http/hls/start_plan.rs",
    "owner_plan",
    "http/internal_media_sessions.rs",
    "start",
)];

/// How a helper is called: bare in its own file and `module::name(`
/// elsewhere, or as a method on a field of this name.
const METHOD_HELPERS: &[(&str, &str)] = &[("media_pool.rs", "media_pool")];

/// Files and directories that hold fences, ownership, admission, workers or
/// boot: none may read the view or call a helper that does.
const STAYS: &[&str] = &[
    "backup.rs",
    "cachekeep.rs",
    "clock_offset.rs",
    "http/auth.rs",
    "http/hls/control.rs",
    "http/hls/preparation.rs",
    "http/internal_media_sessions.rs",
    "http/offline.rs",
    "http/remote.rs",
    "http/remote/",
    "http/shared_receiver_control.rs",
    "http/users.rs",
    "main.rs",
    "media_sessions.rs",
    "media_sessions/",
    "offline.rs",
    "playback_cache.rs",
    "serving_fence.rs",
    "source_probe/",
    "state.rs",
    "telemetry.rs",
    "transcode.rs",
    "transcode/",
    "transcoder_optimization.rs",
    "transcoder_optimization/",
    "vod/",
    "vodencode.rs",
    "vodserve.rs",
];

const VIEW_METHODS: &[&str] = &[
    "planning",
    "planning_from_authority",
    "setting",
    "setting_pair",
    "setting_values",
    "settings",
    "settings_snapshot",
    "switch",
];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn is_test_path(rel: &str) -> bool {
    let stem = rel
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".rs");
    rel.split('/').any(|part| part == "tests")
        || stem == "tests"
        || stem.ends_with("_tests")
        || stem.ends_with("_test")
        || stem.starts_with("test_")
        || rel == "http/applied_settings_inventory.rs"
        || rel == "http/applied_route_inventory.rs"
}

/// Every production Rust file under `src/`, masked by [`production`].
pub(super) fn sources() -> Vec<(String, String)> {
    fn walk(path: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).expect("read a source directory") {
            let path = entry.expect("a source entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let root = src_root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let rel = path
                .strip_prefix(&root)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            (!is_test_path(&rel)).then(|| {
                let text = std::fs::read_to_string(&path).expect("read a source file");
                (rel, production(&mask(&text)))
            })
        })
        .collect()
}

fn blank(out: &mut [u8], from: usize, to: usize) {
    let to = to.min(out.len());
    for byte in &mut out[from..to] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// The source with comment and literal contents blanked, offsets kept, so
/// nothing inside a string or a comment can look like code.
pub(super) fn mask(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"//") {
            let end = rest
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |e| i + e);
            blank(&mut out, i, end);
            i = end;
        } else if rest.starts_with(b"/*") {
            let end = src[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |e| i + 2 + e + 2);
            blank(&mut out, i, end);
            i = end;
        } else if (rest.starts_with(b"r\"") || rest.starts_with(b"r#") || rest.starts_with(b"br"))
            && (i == 0 || !ident(bytes[i - 1]))
            && {
                let after = if rest.starts_with(b"br") {
                    &rest[2..]
                } else {
                    &rest[1..]
                };
                let hashes = after.iter().take_while(|b| **b == b'#').count();
                after.get(hashes) == Some(&b'"')
            }
        {
            let prefix = if rest.starts_with(b"br") { 2 } else { 1 };
            let hashes = rest[prefix..].iter().take_while(|b| **b == b'#').count();
            let open = i + prefix + hashes + 1;
            let close = format!("\"{}", "#".repeat(hashes));
            let end = src[open..].find(&close).map_or(bytes.len(), |e| open + e);
            blank(&mut out, open, end);
            i = end + close.len();
        } else if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            blank(&mut out, i + 1, j);
            i = j + 1;
        } else if bytes[i] == b'\'' {
            let close = if bytes.get(i + 1) == Some(&b'\\') {
                src[i + 2..]
                    .find('\'')
                    .filter(|e| *e <= 10)
                    .map(|e| i + 2 + e)
            } else {
                src[i + 1..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .filter(|width| bytes.get(i + 1 + width) == Some(&b'\''))
                    .map(|width| i + 1 + width)
            };
            match close {
                Some(end) => {
                    blank(&mut out, i + 1, end);
                    i = end + 1;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("masking keeps ASCII and whole characters")
}

/// A masked source with every `#[cfg(test)]` item blanked.
pub(super) fn production(masked: &str) -> String {
    let mut out = masked.as_bytes().to_vec();
    let mut at = 0;
    while let Some(offset) = masked[at..].find("#[cfg(test)]") {
        let start = at + offset;
        let item = start + "#[cfg(test)]".len();
        let semi = masked[item..].find(';').map(|e| item + e);
        let brace = masked[item..].find('{').map(|e| item + e);
        let end = match (semi, brace) {
            (Some(semi), Some(brace)) if semi < brace => semi + 1,
            (_, Some(brace)) => {
                let mut depth = 0_usize;
                let mut end = masked.len();
                for (index, byte) in masked.bytes().enumerate().skip(brace) {
                    match byte {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = index + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                end
            }
            (Some(semi), None) => semi + 1,
            (None, None) => masked.len(),
        };
        blank(&mut out, start, end);
        at = end.max(item);
    }
    String::from_utf8(out).expect("blanking keeps ASCII")
}

pub(super) fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The identifier ending just before `pos` (whitespace skipped).
pub(super) fn ident_before(text: &str, pos: usize) -> &str {
    let bytes = text.as_bytes();
    let mut end = pos;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    &text[start..end]
}

/// The function a position sits in: the last `fn name` before it.
pub(super) fn enclosing(text: &str, pos: usize) -> String {
    let bytes = text.as_bytes();
    let mut search = pos;
    while let Some(found) = text[..search].rfind("fn ") {
        if found == 0 || !is_ident(bytes[found - 1]) {
            let name: String = text[found + 3..]
                .trim_start()
                .bytes()
                .take_while(|b| is_ident(*b))
                .map(char::from)
                .collect();
            if !name.is_empty() {
                return name;
            }
        }
        search = found;
    }
    String::new()
}

/// `(position, method)` of every `.method(` call in `text`, with the
/// receiver identifier.
pub(super) fn method_calls<'a>(text: &'a str, names: &[&'a str]) -> Vec<(usize, &'a str, &'a str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for (dot, _) in text.match_indices('.') {
        let rest = text[dot + 1..].trim_start();
        let name_start = text.len() - rest.len();
        let name_len = rest.bytes().take_while(|b| is_ident(*b)).count();
        let name = &text[name_start..name_start + name_len];
        if !names.contains(&name) || !rest[name_len..].trim_start().starts_with('(') {
            continue;
        }
        if dot > 0 && bytes[dot - 1] == b'.' {
            continue;
        }
        out.push((dot, name, ident_before(text, dot)));
    }
    out
}

/// Direct reads of the view: `applied.<method>(`. Every handle is named
/// `applied` (pinned below), so the receiver sees them all.
fn view_reads(sources: &[(String, String)]) -> BTreeMap<(String, String), usize> {
    let mut reads = BTreeMap::new();
    for (path, text) in sources {
        for (dot, _, receiver) in method_calls(text, VIEW_METHODS) {
            if receiver == "applied" {
                *reads
                    .entry((path.clone(), enclosing(text, dot)))
                    .or_default() += 1;
            }
        }
    }
    reads
}

fn module_of(path: &str) -> &str {
    let file = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".rs");
    if file == "mod" {
        path.rsplit('/').nth(1).unwrap_or(file)
    } else {
        file
    }
}

/// The direct callers of `helper` (defined in `file`).
fn callers(sources: &[(String, String)], file: &str, helper: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let method = METHOD_HELPERS
        .iter()
        .find(|(defining, _)| *defining == file)
        .map(|(_, receiver)| *receiver);
    for (path, text) in sources {
        let bytes = text.as_bytes();
        if let Some(receiver) = method {
            for (dot, _, before) in method_calls(text, &[helper]) {
                if before == receiver {
                    out.insert((path.clone(), enclosing(text, dot)));
                }
            }
            continue;
        }
        for (pos, _) in text.match_indices(helper) {
            let end = pos + helper.len();
            if (pos > 0 && is_ident(bytes[pos - 1])) || !text[end..].trim_start().starts_with('(') {
                continue;
            }
            let before = text[..pos].trim_end();
            if before.ends_with("fn") || before.ends_with('.') {
                continue;
            }
            let qualified = before.strip_suffix("::").map(|q| ident_before(q, q.len()));
            let counts = match qualified {
                Some(module) => module == module_of(file),
                None => path == file,
            };
            let caller = enclosing(text, pos);
            if counts && !(path == file && caller == helper) {
                out.insert((path.clone(), caller));
            }
        }
    }
    out
}

fn in_stays(path: &str) -> bool {
    STAYS
        .iter()
        .any(|stay| path == *stay || (stay.ends_with('/') && path.starts_with(stay)))
}

#[test]
fn every_settings_view_read_and_its_callers_are_pinned_by_function() {
    let sources = sources();
    let reads = view_reads(&sources);
    let pinned: BTreeMap<(String, String), usize> = VIEW_READS
        .iter()
        .map(|(file, function, count, _)| (((*file).to_owned(), (*function).to_owned()), *count))
        .collect();
    let mut found_callers = BTreeSet::new();
    for (file, function, _, helper) in VIEW_READS {
        if *helper {
            for (path, caller) in callers(&sources, file, function) {
                found_callers.insert(((*file).to_owned(), (*function).to_owned(), path, caller));
            }
        }
    }
    let pinned_callers: BTreeSet<(String, String, String, String)> = HELPER_CALLERS
        .iter()
        .map(|(a, b, c, d)| ((*a).into(), (*b).into(), (*c).into(), (*d).into()))
        .collect();
    let render_reads: String = reads
        .iter()
        .map(|((file, function), count)| format!("    (\"{file}\", \"{function}\", {count}, ?),\n"))
        .collect();
    let render_callers: String = found_callers
        .iter()
        .map(|(a, b, c, d)| format!("    (\"{a}\", \"{b}\", \"{c}\", \"{d}\"),\n"))
        .collect();
    assert!(
        reads == pinned && found_callers == pinned_callers,
        "the settings view inventory moved (contract §6.3); found:\nVIEW_READS\n{render_reads}\
         HELPER_CALLERS\n{render_callers}"
    );
    for (path, function) in reads.keys() {
        assert!(
            !in_stays(path),
            "{path}::{function} reads the view in a stays file"
        );
    }
    for (file, helper, path, caller) in &found_callers {
        let pinned = STAYS_CHAINS.iter().any(|(a, b, c, d)| {
            (*a, *b, *c, *d)
                == (
                    file.as_str(),
                    helper.as_str(),
                    path.as_str(),
                    caller.as_str(),
                )
        });
        assert!(
            !in_stays(path) || pinned,
            "{path}::{caller} reaches the view through {helper} from a stays file"
        );
    }
    // Every pinned exception is live, and the helper it names is not itself
    // in a stays file.
    for (file, helper, path, caller) in STAYS_CHAINS {
        assert!(!in_stays(file), "{file}::{helper} sits in a stays file");
        assert!(
            found_callers.contains(&(
                (*file).to_owned(),
                (*helper).to_owned(),
                (*path).to_owned(),
                (*caller).to_owned()
            )),
            "stale stays chain {path}::{caller} -> {helper}"
        );
    }
}

/// The helpers that take the create's plan (D3b, contract §4.2): each takes
/// `plan: Option<&StartPlan>` as its last argument, and `None` reads the
/// Store exactly as before.
const PLANNED_HELPERS: &[&str] = &[
    "cluster_playlist_is_typeless",
    "create_cluster_session",
    "create_cluster_session_for_receiver",
    "create_cluster_session_with_priority",
    "create_session_inner",
    "prepare_vod_encoding_noting",
    "prepare_vod_encoding_with_source",
    "prepare_vod_encoding_with_source_scoped",
    "refuse_unadmittable_encoding",
    "remote_placement_ready",
    "resolve_held_movie_plan_with_fonts",
    "resolve_movie_plan_with_processing_exclusion",
    "resolve_vod_movie_plan_with_fonts",
    "restore_candidate_context_with_deadline",
    "restore_catalog_context_planned",
    "rolling_start_inputs",
    "software_budget_planned",
    "start_copy_with_audio_offset",
    "start_live_recovery_session",
    "start_with_audio_offset",
    "try_vod_session",
    "validate_candidate_planning_binding",
    "validate_hevc_copy_transport",
    "vod_preparation_snapshot_planned",
    "vod_settings_planned",
];

/// Where a plan comes from: the ingress create and the remote owner's start,
/// the two functions that read one (§4.2, pinned by `HELPER_CALLERS`).
const PLAN_ORIGINS: &[(&str, &str)] = &[
    ("http/hls/create.rs", "create_with_purpose_inner"),
    ("http/internal_media_sessions.rs", "start"),
];

/// The caller chains of §6.1, by call: `(file, caller, helper, plan
/// argument)` for every production call of a planned helper. `None` is a
/// caller that is not a create (takeover, resurrection, a prepared
/// successor, a processed upgrade, a Source preparation, the Live TV
/// worker choice, a compatibility wrapper); anything else forwards the
/// caller's own `plan` or, in a plan origin, hands it over.
const PLAN_CALLS: &[(&str, &str, &str, &str)] = &[];

/// The last top-level argument of the call whose `(` is at `open`, with its
/// whitespace collapsed.
fn last_argument(text: &str, open: usize) -> String {
    let bytes = text.as_bytes();
    let mut depth = 0_i32;
    let mut start = open + 1;
    let mut last = String::new();
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    let arg = text[start..index].trim();
                    if !arg.is_empty() {
                        last = arg.to_owned();
                    }
                    break;
                }
            }
            b',' if depth == 1 => {
                let arg = text[start..index].trim();
                if !arg.is_empty() {
                    last = arg.to_owned();
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    last.split_whitespace().collect::<Vec<_>>().join("")
}

#[test]
fn every_plan_argument_is_pinned_by_call() {
    let mut found = BTreeSet::new();
    for (path, text) in sources() {
        let bytes = text.as_bytes();
        for helper in PLANNED_HELPERS {
            for (pos, _) in text.match_indices(helper) {
                let end = pos + helper.len();
                if (pos > 0 && is_ident(bytes[pos - 1]))
                    || bytes.get(end).is_some_and(|byte| is_ident(*byte))
                {
                    continue;
                }
                let rest = &text[end..];
                if !rest.trim_start().starts_with('(') {
                    continue;
                }
                if text[..pos].trim_end().ends_with("fn") {
                    continue;
                }
                let open = end + (rest.len() - rest.trim_start().len());
                found.insert((
                    path.clone(),
                    enclosing(&text, pos),
                    (*helper).to_owned(),
                    last_argument(&text, open),
                ));
            }
        }
    }
    let pinned: BTreeSet<(String, String, String, String)> = PLAN_CALLS
        .iter()
        .map(|(a, b, c, d)| ((*a).into(), (*b).into(), (*c).into(), (*d).into()))
        .collect();
    let render: String = found
        .iter()
        .map(|(a, b, c, d)| format!("    (\"{a}\", \"{b}\", \"{c}\", \"{d}\"),\n"))
        .collect();
    assert!(
        found == pinned,
        "the plan's caller chains moved (contract §6.1); found:\nPLAN_CALLS\n{render}"
    );
    for (path, caller, helper, argument) in &found {
        let origin = PLAN_ORIGINS
            .iter()
            .any(|(file, function)| file == path && function == caller);
        assert!(
            argument == "None" || argument.starts_with("plan") || origin,
            "{path}::{caller} hands {helper} a plan it did not receive: {argument}"
        );
        if argument != "None" && !argument.starts_with("plan") {
            assert!(origin, "{path}::{caller}: {argument}");
        }
    }
}

/// Every applied-view handle is spelled `applied`, so the receiver check
/// above sees every read.
#[test]
fn every_applied_view_handle_is_named_applied() {
    for (path, text) in sources() {
        for line in text.lines() {
            let Some(at) = line.find("AppliedView") else {
                continue;
            };
            let after = &line[at + "AppliedView".len()..];
            if after.bytes().next().is_some_and(is_ident) {
                continue; // AppliedViewMetrics, AppliedViewObservation
            }
            let before = line[..at]
                .trim_end_matches("plurx_core::store::")
                .trim_end()
                .trim_end_matches('&')
                .trim_end();
            if let Some(binding) = before.strip_suffix(':') {
                let name = ident_before(binding, binding.len());
                assert_eq!(
                    name, "applied",
                    "{path}: an AppliedView named `{name}`: {line}"
                );
            }
            if let Some(rest) = line.trim_start().strip_prefix("let ") {
                let name: String = rest
                    .trim_start_matches("mut ")
                    .bytes()
                    .take_while(|b| is_ident(*b))
                    .map(char::from)
                    .collect();
                assert_eq!(
                    name, "applied",
                    "{path}: an AppliedView bound as `{name}`: {line}"
                );
            }
        }
    }
}

/// The reads that stay quorum reads by the contract, where they are.
#[test]
fn the_save_response_fences_and_admission_stay_quorum_reads() {
    let sources: BTreeMap<String, String> = sources().into_iter().collect();
    let body = |path: &str, function: &str| -> String {
        let text = &sources[path];
        let start = text
            .find(&format!("fn {function}("))
            .unwrap_or_else(|| panic!("{path}::{function}"));
        let brace = start + text[start..].find('{').expect("a body");
        let mut depth = 0_usize;
        for (index, byte) in text.bytes().enumerate().skip(brace) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return text[start..=index]
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect();
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced {path}::{function}")
    };
    // GET reads the view; the PUT response is the one Authority snapshot.
    let get = body("http/system.rs", "get_settings");
    assert!(get.contains("settings_dto(&state,SettingsDtoSource::View)"));
    let update = body("http/system.rs", "update_settings");
    assert_eq!(
        update
            .matches("settings_dto(&state,SettingsDtoSource::Authority)")
            .count(),
        1
    );
    assert_eq!(
        update.matches("state.store.settings_snapshot()").count(),
        3,
        "the save handler's three pre-mutation snapshots"
    );
    let dto = body("http/system.rs", "settings_dto");
    assert!(dto.contains(
        "SettingsDtoSource::Authority=>Arc::new(state.store.settings_snapshot().await?)"
    ));
    // Remote placement admits a forwarded start before it allocates.
    let placement = body("media_pool.rs", "remote_placement_ready");
    assert!(placement
        .contains("state.store.get_setting(plurx_core::store::keys::CLUSTER_MEDIA_POOL_ENABLED)"));
    // `PREPARED_QUALITY_HANDOFF` under the transition lock a save takes.
    for (path, count) in [
        ("http/hls/preparation.rs", 2),
        ("http/shared_receiver_control.rs", 1),
    ] {
        let compact: String = sources[path]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert_eq!(
            compact
                .matches(".get_setting(plurx_core::store::keys::PREPARED_QUALITY_HANDOFF)")
                .count(),
            count,
            "{path}"
        );
    }
    // Takeover's live-recovery switch is an ownership step.
    let recovery = body("transcode/manager/create.rs", "live_hls_recovery_enabled");
    assert!(recovery.contains(".store.get_setting(plurx_core::store::keys::VOD_LIVE_RECOVERY)"));
}

#[test]
fn the_census_masks_literals_and_skips_test_items() {
    let source = "fn a() { state.applied.setting(k); s.applied.settings_snapshot(); }\n\
                  fn b() { let x = \"applied.setting(\"; // applied.setting(\n }\n\
                  #[cfg(test)]\nmod tests {\n    fn t() { state.applied.setting(k); { } }\n}\n\
                  #[cfg(test)]\nfn helper() { state.applied.setting(k); }\n\
                  fn c() { store.settings_snapshot(); x.settings(); view.switch(k, true); }\n\
                  fn d() { state\n    .applied\n    .switch(k, true); }\n";
    let text = production(&mask(source));
    let reads = view_reads(&[("x.rs".to_owned(), text)]);
    let expected: BTreeMap<(String, String), usize> = [
        (("x.rs".to_owned(), "a".to_owned()), 2),
        (("x.rs".to_owned(), "d".to_owned()), 1),
    ]
    .into_iter()
    .collect();
    assert_eq!(reads, expected);
}
