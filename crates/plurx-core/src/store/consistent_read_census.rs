//! A source census of the consistent reads in the replicated store (K-04 M3).
//!
//! Every `query_consistent*` call in a `hiqlite*.rs` slice is one quorum
//! round trip through the leader whenever it runs. The bounded-replica plan
//! lowers the per-request number of those by moving eligible reads behind
//! `CatalogueReader` and by answering two reads with one; this census only
//! keeps the other side of that ledger honest, so a new consistent read
//! cannot land without someone deciding it must be one.
//!
//! It is a **source census**, nothing more. It counts call sites in the
//! source text, not requests, round trips, or latency: a site inside a loop
//! counts once, and a site no route reaches counts the same as the one on
//! Home. The per-request number is what `plurx_http_store_reads_total`
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
    ("hiqlite_durable.rs", 28),
    ("hiqlite_dv_conversion.rs", 13),
    ("hiqlite_dvr.rs", 19),
    ("hiqlite_fragment_index_cluster.rs", 24),
    ("hiqlite_import.rs", 3),
    ("hiqlite_library_channels.rs", 13),
    ("hiqlite_media.rs", 60),
    ("hiqlite_pretranscode.rs", 4),
    ("hiqlite_publication.rs", 5),
    ("hiqlite_reading.rs", 2),
    ("hiqlite_sessions.rs", 18),
    ("hiqlite_shared_cache.rs", 4),
    ("hiqlite_timeline_annotations.rs", 2),
];

/// Unannotated sites across every slice when this census was introduced.
const UNANNOTATED_CEILING: usize = 226;

const SITE: &str = "query_consistent";
const REASON: &str = "// authority:";

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
            std::iter::repeat_n(reason, line.matches(SITE).count())
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
