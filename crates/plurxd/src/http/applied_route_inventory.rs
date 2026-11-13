//! The route call-site inventory of playback-start E1c
//! (`docs/streaming/PLAYBACK-START-E1-BUILD.md` §6.1, §6.2), pinned over
//! every production file of this crate in the style of
//! `applied_settings_inventory.rs`.
//!
//! Three routes reach the applied view: ordinary routing (`relay_if_remote`
//! through the cached lookup), the first status resolution (view first, a
//! quorum read confirming a verdict that is not active) and the Activity
//! observer. Everything else that reads a session-keyed route — final
//! verdicts, control exchanges, the status bracket and reclassification, the
//! receipt fences and the five receipt reads §6.1 keeps by default — stays a
//! Store read. The tests pin, by function and by the site it is counted
//! under: every Store read of a session-keyed route, every read through the
//! view, every caller of the cached lookups, and that the Store method itself
//! is called from one place. A moved fence fails here: classify it against
//! §6.1, then update the pin and say why in the PR.

use super::applied_settings_inventory::{enclosing, is_ident, method_calls, sources};
use std::collections::BTreeMap;

/// Every Store (quorum) read of a session-keyed route:
/// `(file, function, site, count)`. `site` is the `RouteReadSite` named at
/// the call, or `site` where the function passes its caller's.
const STORE_READS: &[(&str, &str, &str, usize)] = &[
    // Fences before a claim or durable observation (§6.1, stay).
    (
        "http/hls/candidate_recovery.rs",
        "accept_sample",
        "ReceiptFence",
        1,
    ),
    ("http/hls/link_receipts.rs", "accept", "ReceiptFence", 1),
    ("http/hls/prepared_link.rs", "register", "ReceiptFence", 1),
    // The five receipt reads §6.1 lets E1c move only on the baseline's
    // numbers; the fleet baseline put them at 8% of route reads (stay).
    ("http/hls/link_receipts.rs", "accept", "ReceiptCandidate", 1),
    (
        "http/hls/link_receipts.rs",
        "accept_negative_for_ack",
        "ReceiptCandidate",
        1,
    ),
    (
        "http/hls/link_receipts.rs",
        "current_positive_until",
        "ReceiptCandidate",
        2,
    ),
    (
        "http/hls/prepared_link.rs",
        "register",
        "ReceiptCandidate",
        1,
    ),
    // Ownership and terminal proofs, reads before a mutation (stay).
    (
        "http/hls/create.rs",
        "create_with_purpose_inner",
        "Verdict",
        1,
    ),
    (
        "http/internal_media_sessions.rs",
        "control_authorized",
        "Verdict",
        1,
    ),
    (
        "http/jellyfin/playback.rs",
        "bind_native_start",
        "Verdict",
        1,
    ),
    (
        "http/jellyfin/playback.rs",
        "unclaimed_native_start",
        "Verdict",
        1,
    ),
    (
        "http/shared_receiver_direct.rs",
        "receiver_direct",
        "Verdict",
        1,
    ),
    (
        "http/shared_receiver_playback.rs",
        "receiver_media",
        "Verdict",
        1,
    ),
    (
        "vod/serve/delivery.rs",
        "bind_family_description_before",
        "Verdict",
        1,
    ),
    (
        "vod/shared.rs",
        "terminal_route_durably_non_live",
        "Verdict",
        1,
    ),
    // Control exchanges.
    ("media_sessions.rs", "control_route", "Control", 1),
    ("playback_control.rs", "verify_authority", "Control", 1),
    // Final verdicts and the status bracket/reclassification, by site.
    (
        "media_sessions.rs",
        "authoritative_route_resolution_at",
        "site",
        1,
    ),
];

/// Every quorum read the route cache makes through the view handle to
/// replace an `Observed` fact past its lease.
const QUORUM_READS: &[(&str, &str, &str, usize)] =
    &[("media_sessions.rs", "raw_route_before", "site", 1)];

/// Every read through the applied view: `(file, function, site, count)`.
const VIEW_READS: &[(&str, &str, &str, usize)] = &[
    // The cached lookup (routing and status first pass their site).
    ("media_sessions.rs", "raw_route_before", "site", 1),
    // The Activity detail: read-only.
    (
        "http/system.rs",
        "prepared_seam_session_processed",
        "Observer",
        1,
    ),
];

/// Every caller of the cached lookups, which read through the view:
/// `(file, function, method, count)`.
const CACHED_LOOKUP_CALLERS: &[(&str, &str, &str, usize)] = &[
    (
        "http/hls/relay.rs",
        "relay_if_remote",
        "media_route_resolution_before",
        1,
    ),
    (
        "http/hls/relay.rs",
        "relay_if_remote",
        "route_resolution_before",
        1,
    ),
    (
        "http/hls/status.rs",
        "status_local_answer",
        "status_view_media_route_resolution_before",
        1,
    ),
];

const CACHED_LOOKUPS: &[&str] = &[
    "media_route_resolution_before",
    "route_resolution_before",
    "status_view_media_route_resolution_before",
];

/// Production sources without the test fixtures `http/mod.rs` declares under
/// `#[cfg(test)]` (their file names say so).
fn production_sources() -> Vec<(String, String)> {
    sources()
        .into_iter()
        .filter(|(path, _)| !path.ends_with("_fixture.rs"))
        .collect()
}

/// The argument text of the call whose `(` is at `open`.
fn arguments(text: &str, open: usize) -> &str {
    let mut depth = 0_usize;
    for (index, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + 1..index];
                }
            }
            _ => {}
        }
    }
    &text[open + 1..]
}

/// The site a call names: `RouteReadSite::<X>`, or `site` passed through.
fn site_of(arguments: &str) -> String {
    if let Some(at) = arguments.find("RouteReadSite::") {
        return arguments[at + "RouteReadSite::".len()..]
            .bytes()
            .take_while(|byte| is_ident(*byte))
            .map(char::from)
            .collect();
    }
    let bytes = arguments.as_bytes();
    let passes_site = arguments.match_indices("site").any(|(at, _)| {
        (at == 0 || !is_ident(bytes[at - 1]))
            && bytes.get(at + 4).is_none_or(|byte| !is_ident(*byte))
    });
    if passes_site { "site" } else { "?" }.to_owned()
}

/// Free-function calls of `name(` (bare or path-qualified), by function and
/// site.
fn calls(sources: &[(String, String)], name: &str) -> BTreeMap<(String, String, String), usize> {
    let mut out = BTreeMap::new();
    for (path, text) in sources {
        let bytes = text.as_bytes();
        for (at, _) in text.match_indices(name) {
            let end = at + name.len();
            if at > 0 && is_ident(bytes[at - 1]) {
                continue;
            }
            let rest = &text[end..];
            if !rest.starts_with('(') {
                continue;
            }
            if text[..at].trim_end().ends_with("fn") {
                continue;
            }
            let site = site_of(arguments(text, end));
            *out.entry((path.clone(), enclosing(text, at), site))
                .or_default() += 1;
        }
    }
    out
}

fn pinned(rows: &[(&str, &str, &str, usize)]) -> BTreeMap<(String, String, String), usize> {
    rows.iter()
        .map(|(file, function, site, count)| {
            (
                (
                    (*file).to_owned(),
                    (*function).to_owned(),
                    (*site).to_owned(),
                ),
                *count,
            )
        })
        .collect()
}

fn render(found: &BTreeMap<(String, String, String), usize>) -> String {
    found
        .iter()
        .map(|((file, function, site), count)| {
            format!("    (\"{file}\", \"{function}\", \"{site}\", {count}),\n")
        })
        .collect()
}

#[test]
fn every_route_read_is_pinned_by_function_and_site() {
    let sources = production_sources();
    let store = calls(&sources, "read_media_session_route");
    let view = calls(&sources, "read_media_session_route_view");
    let quorum = calls(&sources, "read_media_session_route_quorum");
    assert!(
        store == pinned(STORE_READS)
            && view == pinned(VIEW_READS)
            && quorum == pinned(QUORUM_READS),
        "the route inventory moved (contract §6.1); found:\nSTORE_READS\n{}VIEW_READS\n{}QUORUM_READS\n{}",
        render(&store),
        render(&view),
        render(&quorum)
    );
}

#[test]
fn every_caller_of_the_cached_route_lookups_is_pinned() {
    let mut found = BTreeMap::new();
    for (path, text) in production_sources() {
        for (dot, name, _) in method_calls(&text, CACHED_LOOKUPS) {
            *found
                .entry((path.clone(), enclosing(&text, dot), name.to_owned()))
                .or_default() += 1;
        }
    }
    assert!(
        found == pinned(CACHED_LOOKUP_CALLERS),
        "a caller of the cached route lookups moved (contract §6.1); found:\n{}",
        render(&found)
    );
}

/// The Store method is called from the one counted reader, and the view's
/// route and quorum methods from the counted view readers, in any spelling —
/// method call or path-qualified — so no read escapes the pins above.
#[test]
fn the_store_and_view_route_methods_are_called_from_one_place_each() {
    let mut found = BTreeMap::new();
    for (path, text) in production_sources() {
        for (dot, name, receiver) in method_calls(
            &text,
            &[
                "media_session_route",
                "route",
                "quorum_route",
                "authority_route",
            ],
        ) {
            if name == "route" && receiver != "applied" {
                continue;
            }
            *found
                .entry((path.clone(), enclosing(&text, dot), name.to_owned()))
                .or_default() += 1;
        }
        for qualified in [
            "AppliedView::route(",
            "RouteView::route(",
            "AppliedView::authority_route(",
            "RouteView::quorum_route(",
        ] {
            for (at, _) in text.match_indices(qualified) {
                *found
                    .entry((path.clone(), enclosing(&text, at), qualified.to_owned()))
                    .or_default() += 1;
            }
        }
    }
    let expected = pinned(&[
        (
            "media_sessions.rs",
            "read_media_session_route",
            "media_session_route",
            1,
        ),
        (
            "media_sessions.rs",
            "read_media_session_route_view",
            "route",
            1,
        ),
        (
            "media_sessions.rs",
            "read_media_session_route_quorum",
            "quorum_route",
            1,
        ),
        // `impl RouteView for AppliedView`: the trait methods forward.
        ("media_sessions.rs", "route", "AppliedView::route(", 1),
        (
            "media_sessions.rs",
            "quorum_route",
            "AppliedView::authority_route(",
            1,
        ),
    ]);
    assert!(
        found == expected,
        "a route read bypasses the counted readers; found:\n{}",
        render(&found)
    );
}

/// Every route-cache and applied-view entry point is called from a pinned
/// function (review S2): a fence that reached the cache or the view through
/// a private helper would add a caller here. `self.applied`, `self.routes`
/// and `dyn RouteView` are counted where they appear in `media_sessions.rs`,
/// and every `dyn RouteView` handle is named `applied`.
#[test]
fn every_route_cache_entry_point_has_a_pinned_caller() {
    const ENTRY_POINTS: &[&str] = &[
        "raw_route_before",
        "route_resolution_at",
        "media_route_resolution_at",
        "cached_route",
        "lock_routes",
        "cached_route_is_a_fact",
        "adopt_authoritative_route",
    ];
    let mut found = BTreeMap::new();
    for (path, text) in production_sources() {
        for (dot, name, _) in method_calls(&text, ENTRY_POINTS) {
            *found
                .entry((path.clone(), enclosing(&text, dot), name.to_owned()))
                .or_default() += 1;
        }
        if path != "media_sessions.rs" {
            assert!(
                !text.contains("dyn RouteView"),
                "{path} holds a RouteView handle"
            );
            continue;
        }
        let bytes = text.as_bytes();
        for handle in ["self.applied", "self.routes", "dyn RouteView"] {
            for (at, _) in text.match_indices(handle) {
                if bytes
                    .get(at + handle.len())
                    .is_some_and(|byte| is_ident(*byte))
                {
                    continue;
                }
                *found
                    .entry((path.clone(), enclosing(&text, at), handle.to_owned()))
                    .or_default() += 1;
            }
        }
        for line in text.lines().filter(|line| line.contains("dyn RouteView")) {
            let binding = line.split(':').next().unwrap_or_default().trim();
            assert_eq!(
                binding, "applied",
                "a RouteView handle not named applied: {line}"
            );
        }
    }
    let expected = pinned(&[
        (
            "http/hls/relay.rs",
            "relay_if_remote",
            "adopt_authoritative_route",
            2,
        ),
        (
            "http/hls/relay.rs",
            "relay_if_remote",
            "cached_route_is_a_fact",
            1,
        ),
        (
            "http/hls/status.rs",
            "status_local_answer",
            "adopt_authoritative_route",
            1,
        ),
        (
            "media_sessions.rs",
            "adopt_authoritative_route",
            "lock_routes",
            1,
        ),
        (
            "media_sessions.rs",
            "cache_queried_route_result",
            "lock_routes",
            1,
        ),
        (
            "media_sessions.rs",
            "cache_route_if_generation",
            "lock_routes",
            1,
        ),
        ("media_sessions.rs", "cache_route_result", "lock_routes", 1),
        ("media_sessions.rs", "cached_route", "lock_routes", 1),
        ("media_sessions.rs", "cached_route", "self.applied", 4),
        (
            "media_sessions.rs",
            "cached_route_is_a_fact",
            "lock_routes",
            1,
        ),
        (
            "media_sessions.rs",
            "insert_cached_route",
            "self.applied",
            4,
        ),
        ("media_sessions.rs", "lock_routes", "self.routes", 1),
        (
            "media_sessions.rs",
            "media_route_resolution_at",
            "route_resolution_at",
            1,
        ),
        (
            "media_sessions.rs",
            "media_route_resolution_before",
            "media_route_resolution_at",
            1,
        ),
        ("media_sessions.rs", "observed_now", "self.applied", 1),
        ("media_sessions.rs", "raw_route_before", "cached_route", 2),
        ("media_sessions.rs", "raw_route_before", "self.applied", 2),
        (
            "media_sessions.rs",
            "read_media_session_route_quorum",
            "dyn RouteView",
            1,
        ),
        (
            "media_sessions.rs",
            "read_media_session_route_view",
            "dyn RouteView",
            1,
        ),
        (
            "media_sessions.rs",
            "route_resolution_at",
            "raw_route_before",
            1,
        ),
        (
            "media_sessions.rs",
            "route_resolution_before",
            "route_resolution_at",
            1,
        ),
        (
            "media_sessions.rs",
            "status_view_media_route_resolution_before",
            "media_route_resolution_at",
            1,
        ),
        // The coordinator's field (counted under the function before it).
        (
            "media_sessions.rs",
            "valid_resource_name",
            "dyn RouteView",
            1,
        ),
        ("media_sessions.rs", "with_applied_view", "dyn RouteView", 1),
    ]);
    assert!(
        found == expected,
        "a route-cache entry point gained or lost a caller; found:\n{}",
        render(&found)
    );
}

/// The first status resolution is view first and confirms a non-active
/// verdict by quorum; the bracket and the reclassification stay quorum
/// reads (§6.1, §12-5).
#[test]
fn status_reads_the_view_first_and_keeps_its_quorum_bracket() {
    let sources: BTreeMap<String, String> = production_sources().into_iter().collect();
    let status: String = sources["http/hls/status.rs"]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert_eq!(
        status
            .matches(".status_view_media_route_resolution_before(session,&state.node_id,request_deadline)")
            .count(),
        1
    );
    for site in ["StatusFirst", "StatusBracket", "StatusReclassify"] {
        assert_eq!(
            status
                .matches(&format!(
                    ".status_media_route_resolution_before(crate::media_sessions::RouteReadSite::{site},"
                ))
                .count(),
            1,
            "{site}"
        );
    }
    // Only an active view answer skips the confirming quorum read.
    assert!(status.contains(
        "active@(DurableRouteResolution::ActiveLocal(_)|DurableRouteResolution::ActiveRemote(_))=>active,_=>{letconfirmed=state"
    ));
}
