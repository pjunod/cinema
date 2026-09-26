//! A source census of the consistent reads in the replicated store (K-04 M3).
//!
//! Every `query_consistent*` call in a `hiqlite*.rs` slice is one quorum
//! round trip through the leader whenever it runs. So is every call that
//! hands a shared dispatch helper a consistent read kind — today
//! `WatchRead::Authority`, which `watch_query` turns into a
//! `query_consistent_map` at one site for every watch read. Counting only the
//! helper's own site would let a new Authority watch read land without the
//! census seeing it, so each such call site counts as a site of its own
//! ([`CONSISTENT_DISPATCH`]); the helper's match arm, which names the kind
//! only to dispatch it, is the `query_consistent` site already counted. The bounded-replica plan
//! lowers the per-request number of those by moving eligible reads behind
//! `CatalogueReader` and by answering two reads with one; this census only
//! keeps the other side of that ledger honest, so a new consistent read
//! cannot land without someone deciding it must be one.
//!
//! It is a **source census**, nothing more. It counts call sites in the
//! source text, not requests, round trips, or latency: a site inside a loop
//! counts once, a helper call that issues two statements counts once, and a
//! site no route reaches counts the same as the one on Home. A new dispatch
//! helper that takes a consistency kind has to add its kind to
//! [`CONSISTENT_DISPATCH`], or its callers are invisible here. The per-request number is what `plurx_http_store_reads_total`
//! measures (K-04 M0), and only that measurement is evidence about latency.
//!
//! Two rules:
//!
//! - each file's count is pinned exactly in [`CONSISTENT_READ_SITES`], so a
//!   file that gains a site fails until the same change raises its entry
//!   (and one that loses a site lowers it, which keeps the ratchet tight);
//! - the total of sites *without* an `// authority: <reason>` comment on the
//!   site's line or the line above may not exceed [`UNANNOTATED_CEILING`],
//!   the count when the census was introduced. Growth past it has to say
//!   why the new read must be linearizable.

use super::placeholder_census::STORE_SOURCES;

/// Consistent-read call sites per replicated slice, production code only.
const CONSISTENT_READ_SITES: &[(&str, usize)] = &[
    ("hiqlite.rs", 26),
    ("hiqlite_catalog.rs", 2),
    ("hiqlite_classification.rs", 1),
    ("hiqlite_coordination.rs", 2),
    ("hiqlite_durable.rs", 29),
    ("hiqlite_dv_conversion.rs", 13),
    ("hiqlite_dvr.rs", 19),
    ("hiqlite_fragment_index_cluster.rs", 32),
    ("hiqlite_import.rs", 3),
    ("hiqlite_library_channels.rs", 13),
    ("hiqlite_media.rs", 67),
    ("hiqlite_pretranscode.rs", 4),
    ("hiqlite_publication.rs", 5),
    ("hiqlite_reading.rs", 2),
    ("hiqlite_sessions.rs", 18),
    ("hiqlite_shared_cache.rs", 4),
    ("hiqlite_timeline_annotations.rs", 2),
];

/// Unannotated sites across every slice when this census was introduced
/// (PR #504, merged with `main` at `448e803da`): 230 `query_consistent`
/// sites at `0e2c3fd47`, eight more that `main` added since (one in
/// `hiqlite_durable.rs`, seven in `hiqlite_fragment_index_cluster.rs`), the
/// two new watch reads (`watch_summary`, `progress_rails`, each one
/// statement standing in for two per request), and `watch_query`'s own
/// dispatch site. Consolidating watch reads lowers reads per request, not
/// sites in the source.
const UNANNOTATED_CEILING: usize = 241;

const SITE: &str = "query_consistent";
const REASON: &str = "// authority:";

/// Consistent read kinds passed to a shared dispatch helper. Each call site
/// that passes one is a consistent-read site; the helper's own match arm
/// (`<kind> =>`) is not, and comment lines are not.
const CONSISTENT_DISPATCH: &[&str] = &["WatchRead::Authority"];

fn dispatch_sites(line: &str) -> usize {
    if line.trim_start().starts_with("//") {
        return 0;
    }
    CONSISTENT_DISPATCH
        .iter()
        .map(|kind| {
            line.match_indices(kind)
                .filter(|(at, _)| !line[at + kind.len()..].trim_start().starts_with("=>"))
                .count()
        })
        .sum()
}

fn production(source: &str) -> &str {
    source
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(source)
}

/// One entry per call site: whether it carries an authority reason.
fn sites(source: &str) -> Vec<bool> {
    let lines: Vec<&str> = production(source).lines().collect();
    let annotated = |line: &str| {
        line.split_once(REASON)
            .is_some_and(|(_, reason)| !reason.trim().is_empty())
    };
    lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| {
            let reason =
                annotated(line) || index.checked_sub(1).is_some_and(|i| annotated(lines[i]));
            std::iter::repeat_n(reason, line.matches(SITE).count() + dispatch_sites(line))
        })
        .collect()
}

fn census(sources: &[(&str, &str)], table: &[(&str, usize)], ceiling: usize) -> Result<(), String> {
    let mut failures = Vec::new();
    let mut unannotated = 0;
    for (name, source) in sources {
        let found = sites(source);
        unannotated += found.iter().filter(|reason| !**reason).count();
        match table.iter().find(|(entry, _)| entry == name) {
            Some((_, pinned)) if *pinned == found.len() => {}
            Some((_, pinned)) => failures.push(format!(
                "{name}: {} consistent-read sites, table pins {pinned}",
                found.len()
            )),
            None => failures.push(format!("{name}: no census entry")),
        }
    }
    if unannotated > ceiling {
        failures.push(format!(
            "{unannotated} consistent-read sites carry no `{REASON} <reason>` comment; \
             the ceiling is {ceiling}"
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

#[test]
fn consistent_read_census_matches_the_checked_in_table() {
    assert_eq!(
        CONSISTENT_READ_SITES.len(),
        STORE_SOURCES.len(),
        "every replicated slice has exactly one census entry"
    );
    census(STORE_SOURCES, CONSISTENT_READ_SITES, UNANNOTATED_CEILING)
        .unwrap_or_else(|failures| panic!("consistent-read census:\n{failures}"));
}

fn with_synthetic_reading_site(comment: &str) -> Vec<(&'static str, String)> {
    STORE_SOURCES
        .iter()
        .map(|(name, source)| {
            let source = if *name == "hiqlite_reading.rs" {
                format!(
                    "{source}\nfn synthetic() {{\n    {comment}\n    \
                     let _ = client.query_consistent_map::<Row, _>(\"SELECT 1\", params!());\n}}\n"
                )
            } else {
                (*source).to_owned()
            };
            (*name, source)
        })
        .collect()
}

fn borrowed<'a>(sources: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    sources
        .iter()
        .map(|(name, source)| (*name, source.as_str()))
        .collect()
}

fn with_synthetic_watch_dispatch() -> Vec<(&'static str, String)> {
    STORE_SOURCES
        .iter()
        .map(|(name, source)| {
            let source = if *name == "hiqlite_media.rs" {
                source.replacen(
                    "impl HiqliteAuthStore {\n    async fn watch_query<T>(",
                    "impl HiqliteAuthStore {\n    async fn synthetic(&self) {\n        \
                     let _ = self.watch_query::<Row>(WatchRead::Authority, \"SELECT 1\", \
                     params!()).await;\n    }\n\n    async fn watch_query<T>(",
                    1,
                )
            } else {
                (*source).to_owned()
            };
            (*name, source)
        })
        .collect()
}

fn raised(name: &str) -> Vec<(&'static str, usize)> {
    CONSISTENT_READ_SITES
        .iter()
        .map(|(entry, count)| (*entry, count + usize::from(*entry == name)))
        .collect()
}

#[test]
fn consistent_read_census_refuses_a_synthetic_extra_read_in_a_fixture_copy() {
    let fixture = with_synthetic_reading_site("");
    let failure = census(
        &borrowed(&fixture),
        CONSISTENT_READ_SITES,
        UNANNOTATED_CEILING,
    )
    .expect_err("an unpinned extra consistent read must fail the census");
    assert!(failure.contains("hiqlite_reading.rs: 3 consistent-read sites, table pins 2"));

    // Raising the file's entry is not enough on its own: the total grows
    // past the ceiling without a stated reason.
    let failure = census(
        &borrowed(&fixture),
        &raised("hiqlite_reading.rs"),
        UNANNOTATED_CEILING,
    )
    .expect_err("an unexplained new consistent read must fail the census");
    assert!(failure.contains("carry no `// authority: <reason>` comment"));
    assert!(!failure.contains("hiqlite_reading.rs:"));
}

/// Review of #504, finding 4: a consistent watch read written through the
/// shared `watch_query` helper adds no `query_consistent` text, and still
/// has to move the census.
#[test]
fn consistent_read_census_refuses_a_new_authority_read_through_a_dispatch_helper() {
    let fixture = with_synthetic_watch_dispatch();
    let (_, media) = fixture
        .iter()
        .find(|(name, _)| *name == "hiqlite_media.rs")
        .expect("media slice");
    assert!(media.contains("async fn synthetic"), "fixture applied");
    let pinned = CONSISTENT_READ_SITES
        .iter()
        .find(|(name, _)| *name == "hiqlite_media.rs")
        .expect("media entry")
        .1;
    let failure = census(
        &borrowed(&fixture),
        CONSISTENT_READ_SITES,
        UNANNOTATED_CEILING,
    )
    .expect_err("a new Authority watch read must fail the census");
    assert!(
        failure.contains(&format!(
            "hiqlite_media.rs: {} consistent-read sites, table pins {pinned}",
            pinned + 1
        )),
        "{failure}"
    );
    // The dispatch arm and comments naming the kind are not sites.
    assert_eq!(
        dispatch_sites("            WatchRead::Authority => self.client().query_consistent_map(sql, params).await,"),
        0
    );
    assert_eq!(dispatch_sites("    // WatchRead::Authority here"), 0);
    assert_eq!(
        dispatch_sites("self.read_watch_map(WatchRead::Authority, user_id, ids)"),
        1
    );
}

#[test]
fn consistent_read_census_admits_a_raised_entry_with_an_authority_reason() {
    let fixture = with_synthetic_reading_site("// authority: a synthetic pre-mutation guard");
    census(
        &borrowed(&fixture),
        &raised("hiqlite_reading.rs"),
        UNANNOTATED_CEILING,
    )
    .unwrap_or_else(|failures| panic!("an explained, pinned read is admitted:\n{failures}"));

    // An empty reason is no reason.
    let fixture = with_synthetic_reading_site("// authority:");
    census(
        &borrowed(&fixture),
        &raised("hiqlite_reading.rs"),
        UNANNOTATED_CEILING,
    )
    .expect_err("an empty authority reason must not admit the read");
}
