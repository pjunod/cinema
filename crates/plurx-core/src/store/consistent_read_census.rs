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
//! Three rules:
//!
//! - every consistent read names the Store method that issued it
//!   ([`unlabelled_sites`]): the first argument of `query_consistent_map*` is a
//!   string literal, or the `method` a wrapper was handed, so a quorum read
//!   cannot reach `plurx_http_store_operations_by_method_total` unattributed.
//!   The label is the method's name (`&'static str`), never a `file:line`;
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
    // Translation, annotations, title jobs and handoffs validate committed
    // extension shape and singleton authority: eight consistent reads.
    // Text translation and shared captions add four more schema/marker reads.
    // Source arbitration verifies both the committed object shape and marker.
    // The contract v2 scan request sidecar does the same: two more.
    // Analysis coordination checks its table marker, canonical objects and
    // protocol singleton before admitting adjunct work: three more.
    // The planning-key extension (D3t) checks its object shape and marker: two more.
    ("hiqlite.rs", 69),
    // Dispatch installation and lost-ack reconciliation share one catalog probe.
    ("hiqlite/dispatch_turns.rs", 1),
    // Two rating-cache reads and three person-owned list membership/page reads.
    ("hiqlite_annotations.rs", 5),
    // Managed bootstrap validates committed shape and the singleton marker.
    ("hiqlite_background_jobs.rs", 4),
    ("hiqlite_catalog.rs", 2),
    ("hiqlite_classification.rs", 1),
    ("hiqlite_coordination.rs", 2),
    ("hiqlite_durable.rs", 29),
    ("hiqlite_dv_conversion.rs", 13),
    ("hiqlite_dvr.rs", 19),
    ("hiqlite_fragment_index_cluster.rs", 35),
    ("hiqlite_import.rs", 4),
    ("hiqlite_remote.rs", 4),
    ("hiqlite_invitations.rs", 22),
    ("hiqlite_jellyfin_catalog.rs", 3),
    ("hiqlite_jellyfin_identity.rs", 3),
    ("hiqlite_jellyfin_login.rs", 2),
    ("hiqlite_jellyfin_play.rs", 4),
    ("hiqlite_library_channels.rs", 13),
    ("hiqlite_live_tv_resource.rs", 1),
    ("hiqlite_media.rs", 72),
    // Handoff status, the redemption verdict, handoff-audience authentication
    // and the token scope each read committed authority; see the
    // `authority:` reasons at the sites.
    ("hiqlite_playback_handoffs.rs", 4),
    ("hiqlite_pretranscode.rs", 2),
    ("hiqlite_publication.rs", 7),
    ("hiqlite_reading.rs", 2),
    ("hiqlite_sessions.rs", 37),
    ("hiqlite_sharing.rs", 1),
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
// Durable queue removes three unannotated legacy execution reads. Its
// authority bridge is separately inventoried and carries its reason.
// E0 removes the legacy subtitle ownership read as well.
// E2 retires the old unfenced manifest-candidate query.
// The architecture effort's authority-read scope split adds two unannotated
// sites that are not reads of their own: `query_consistent_map_scoped`'s
// definition and the unscoped wrapper forwarding to it, both plumbing for the
// one physical read in `hiqlite.rs` that was already counted. Its new reads
// (the audio_recipe shape probe, the two schema-lineage reads, candidate
// recovery memory, the unobserved-file cursor) each carry a reason.
const UNANNOTATED_CEILING: usize = 237;

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

/// The consistent-read call sites whose first argument is neither a string
/// literal nor the forwarded `method` label, as `(line, text)`. A call on the
/// raw vendor client (`self.inner().`, which the one `TimedClient` wrapper
/// makes) is not a site of this rule: it has no label to take.
fn unlabelled_sites(source: &str) -> Vec<(usize, String)> {
    let text = production(source);
    let mut found = Vec::new();
    for (at, _) in text.match_indices(".query_consistent_map") {
        let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
        let line_end = text[at..].find('\n').map_or(text.len(), |index| at + index);
        let line = &text[line_start..line_end];
        if line.trim_start().starts_with("//") || text[..at].trim_end().ends_with("inner()") {
            continue;
        }
        let mut rest = &text[at + ".query_consistent_map".len()..];
        rest = rest.strip_prefix("_scoped").unwrap_or(rest);
        if let Some(turbofish) = rest.strip_prefix("::<") {
            let mut depth = 1_usize;
            let mut end = 0;
            for (index, character) in turbofish.char_indices() {
                match character {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end = index + 1;
                    break;
                }
            }
            rest = &turbofish[end..];
        }
        let Some(arguments) = rest.strip_prefix('(') else {
            continue;
        };
        let first = arguments.trim_start();
        // A label is an identifier-shaped literal, so an SQL literal in the
        // first position (an unlabelled read) is not mistaken for one.
        let labelled = first.strip_prefix('"').is_some_and(|literal| {
            literal.split_once("\",").is_some_and(|(name, _)| {
                !name.is_empty()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        }) || first
            .strip_prefix("method")
            .is_some_and(|after| after.trim_start().starts_with(','));
        if !labelled {
            let number = text[..at].matches('\n').count() + 1;
            found.push((number, line.trim().to_owned()));
        }
    }
    found
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

// Exercise unexplained growth against the measured fixture baseline even
// when production removals leave headroom under the historical ceiling.
fn raised(name: &str) -> Vec<(&'static str, usize)> {
    CONSISTENT_READ_SITES
        .iter()
        .map(|(entry, count)| (*entry, count + usize::from(*entry == name)))
        .collect()
}

// Synthetic controls need a saturated test ceiling even when production gains
// justified reads or removes old sites. The historical production ceiling stays
// unchanged; the fixture cannot silently borrow its unused headroom.
fn saturated_fixture_ceiling() -> usize {
    let current = STORE_SOURCES
        .iter()
        .map(|(_, source)| sites(source).into_iter().filter(|reason| !reason).count())
        .sum();
    assert!(current <= UNANNOTATED_CEILING);
    current
}

#[test]
fn consistent_read_census_refuses_a_synthetic_extra_read_in_a_fixture_copy() {
    let fixture = with_synthetic_reading_site("");
    let failure = census(
        &borrowed(&fixture),
        CONSISTENT_READ_SITES,
        saturated_fixture_ceiling(),
    )
    .expect_err("an unpinned extra consistent read must fail the census");
    assert!(failure.contains("hiqlite_reading.rs: 3 consistent-read sites, table pins 2"));

    // Raising the file's entry is not enough on its own: the total grows
    // past the ceiling without a stated reason.
    let failure = census(
        &borrowed(&fixture),
        &raised("hiqlite_reading.rs"),
        saturated_fixture_ceiling(),
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
        saturated_fixture_ceiling(),
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
        dispatch_sites(
            "            WatchRead::Authority => self.client().query_consistent_map(sql, params).await,"
        ),
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
        saturated_fixture_ceiling(),
    )
    .unwrap_or_else(|failures| panic!("an explained, pinned read is admitted:\n{failures}"));

    // An empty reason is no reason.
    let fixture = with_synthetic_reading_site("// authority:");
    census(
        &borrowed(&fixture),
        &raised("hiqlite_reading.rs"),
        saturated_fixture_ceiling(),
    )
    .expect_err("an empty authority reason must not admit the read");
}

/// Consistent reads made on a raw `hiqlite::Client`, not through
/// `TimedClient`: schema bootstrap, migration and compatibility checks that
/// run before the store serves a request. They never reach
/// `HttpStoreOperationCounts` or the by-method metric, so there is no label
/// for them to take. Pinned per slice: a new raw read fails here until it is
/// either labelled (made through `TimedClient`) or consciously added.
const RAW_CLIENT_SITES: &[(&str, usize)] = &[
    // Watch-column migration, sharing object shape, the cluster compatibility
    // probe and its two committed-marker readers, and two Source/custody
    // shape guards.
    ("hiqlite.rs", 7),
    // The producer payload column shape before a bootstrap ALTER.
    ("hiqlite_background_jobs.rs", 1),
    // The analysis-component and subtitle-source schema currency probes.
    ("hiqlite_fragment_index_cluster.rs", 2),
];

/// The D3a rule (`docs/streaming/PLAYBACK-START-D3-BUILD.md` section 7.1-1):
/// no consistent read without a Store-method label.
#[test]
fn every_consistent_read_names_its_store_method() {
    let mut failures = Vec::new();
    for (name, source) in STORE_SOURCES {
        let found = unlabelled_sites(source);
        let allowed = RAW_CLIENT_SITES
            .iter()
            .find(|(entry, _)| entry == name)
            .map_or(0, |(_, count)| *count);
        if found.len() != allowed {
            failures.push(format!(
                "{name}: {} consistent reads without a method label, {allowed} raw-client reads pinned:\n{}",
                found.len(),
                found
                    .iter()
                    .map(|(line, text)| format!("  {name}:{line}: {text}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_label_rule_refuses_an_unlabelled_read_and_admits_labelled_ones() {
    let unlabelled = "fn f() {\n    client\n        .query_consistent_map::<Row, _>(\"SELECT 1\", params!())\n        .await;\n    client.query_consistent_map::<Map<K, V>, _>(sql, params!());\n}\n";
    let found = unlabelled_sites(unlabelled);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!((found[0].0, found[1].0), (3, 5));
    // A literal, a forwarded label, the scoped variant and the raw client.
    let labelled = "client.query_consistent_map::<Row, _>(\"list_users\", sql, p);\n\
                    client.query_consistent_map(method, sql, p);\n\
                    client.query_consistent_map_scoped(\n    method,\n    sql,\n    p,\n    s);\n\
                    self.inner().query_consistent_map(sql.clone(), p);\n\
                    // client.query_consistent_map(sql, p) in a comment\n";
    assert!(unlabelled_sites(labelled).is_empty());
}
