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
//! Four rules:
//!
//! - every label names a Store method, not the helper that ran it
//!   ([`label_failures`]): a label is a method declared in a `Store` trait, the
//!   `local_` twin of one, or a function listed with its reason in
//!   [`NOT_A_STORE_METHOD`], and a helper takes the caller's label and
//!   forwards it;
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
    // Custody retention (main) checks its trigger shape and marker: two more.
    ("hiqlite.rs", 71),
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
    // #1546: the probe-only scan's removal sweep reads the files under one folder.
    ("hiqlite_media.rs", 73),
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
/// raw vendor client (`self.inner().`, `Client::query_consistent_map(..)`) has
/// no label to take, so it is one of these and is pinned in
/// [`RAW_CLIENT_SITES`]; so is a site this scan cannot parse, so that a
/// spelling it does not understand fails the census instead of escaping it.
fn unlabelled_sites(source: &str) -> Vec<(usize, String)> {
    let text = production(source);
    let mut found = Vec::new();
    let mut calls = text
        .match_indices(".query_consistent_map")
        .chain(text.match_indices("::query_consistent_map"))
        .collect::<Vec<_>>();
    calls.sort_by_key(|(at, _)| *at);
    for (at, _) in calls {
        let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
        let line_end = text[at..].find('\n').map_or(text.len(), |index| at + index);
        let line = &text[line_start..line_end];
        if line.trim_start().starts_with("//") {
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
            let number = text[..at].matches('\n').count() + 1;
            found.push((number, line.trim().to_owned()));
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
/// `TimedClient`: schema bootstrap, migration and compatibility checks, and
/// the one `TimedClient` wrapper itself. They are outside request accounting
/// (a few, such as the committed-schema-version read, can run on a live
/// store, but never inside a request's scope), so there is no label for them
/// to take. Pinned per slice: a new raw read fails here until it is
/// either labelled (made through `TimedClient`) or consciously added.
const RAW_CLIENT_SITES: &[(&str, usize)] = &[
    // Watch-column migration, sharing object shape, the cluster compatibility
    // probe and its two committed-marker readers, two Source/custody shape
    // guards, and the one `TimedClient` wrapper that makes the labelled call.
    ("hiqlite.rs", 8),
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
    // A literal, a forwarded label, the scoped variant and a comment.
    let labelled = "client.query_consistent_map::<Row, _>(\"list_users\", sql, p);\n\
                    client.query_consistent_map(method, sql, p);\n\
                    client.query_consistent_map_scoped(\n    method,\n    sql,\n    p,\n    s);\n\
                    // client.query_consistent_map(sql, p) in a comment\n";
    assert!(unlabelled_sites(labelled).is_empty());
    // The raw vendor client, in both spellings, and a site the scan cannot
    // parse are sites too, so they are pinned rather than skipped.
    let raw = "self.inner().query_consistent_map(sql.clone(), p);\n\
               hiqlite::Client::query_consistent_map(&client, sql, p);\n\
               let read = client.query_consistent_map;\n";
    assert_eq!(
        unlabelled_sites(raw).len(),
        3,
        "{:?}",
        unlabelled_sites(raw)
    );
}

// ---------------------------------------------------------------------------
// The label rule: every label names a Store method (D3a, S1 of the review).
// ---------------------------------------------------------------------------

/// Every source that declares a `Store` trait. Their method names are the
/// vocabulary of `plurx_http_store_operations_by_method_total{method}`.
const STORE_TRAIT_SOURCES: &[&str] = &[
    include_str!("mod.rs"),
    include_str!("background_jobs.rs"),
    include_str!("classification.rs"),
    include_str!("classification_schedule.rs"),
    include_str!("dv_conversion.rs"),
    include_str!("field_order_backfill.rs"),
    include_str!("file_grants.rs"),
    include_str!("fragment_index_cluster.rs"),
    include_str!("invitations.rs"),
    include_str!("jellyfin_catalog.rs"),
    include_str!("jellyfin_identity.rs"),
    include_str!("jellyfin_login.rs"),
    include_str!("jellyfin_play.rs"),
    include_str!("offline_claim.rs"),
    include_str!("offline_expiry.rs"),
    include_str!("playback_handoffs.rs"),
    include_str!("remote.rs"),
    include_str!("sharing.rs"),
    include_str!("sharing_catalogue.rs"),
    include_str!("sharing_catalogue_artwork.rs"),
    include_str!("sharing_catalogue_details.rs"),
    include_str!("sharing_catalogue_source.rs"),
    include_str!("sharing_file_locators.rs"),
    include_str!("sharing_ingress_custody.rs"),
    include_str!("sharing_purpose_keys.rs"),
    include_str!("sharing_receiver_delivery.rs"),
    include_str!("sharing_receiver_ingress.rs"),
    include_str!("sharing_receiver_progress.rs"),
    include_str!("sharing_receiver_retirement.rs"),
    include_str!("sharing_receiver_sessions.rs"),
    include_str!("sharing_source_ingress_custody.rs"),
    include_str!("sharing_source_sessions.rs"),
    include_str!("watched_drain.rs"),
];

/// Labels that are deliberately not a `Store` method, each with why. A label
/// here is a function that is not on a request's path or that is its own
/// operation; a helper that merely stands where a Store method should is not
/// admitted — it takes the label and forwards it. The rule also admits
/// `local_<method>` for a `Store` method (the bounded-replica twin of that
/// method), so those are not listed.
const NOT_A_STORE_METHOD: &[(&str, &str)] = &[
    // Startup, schema migration and compatibility probes: run at bootstrap or
    // on a membership change, outside a request's accounting.
    ("analysis_component_schema_is_current", STARTUP),
    ("analysis_coordination_shape", STARTUP),
    ("bootstrap_with_clock", STARTUP),
    ("bridge_private_lineage_inner", STARTUP),
    ("custody_retention_schema_version", STARTUP),
    ("dispatch_turns_installed", STARTUP),
    ("committed_schema_version_unchecked", STARTUP),
    ("custody_migration_settled", STARTUP),
    ("drain_deadline_column_present", STARTUP),
    ("dv_title_jobs_schema_version", STARTUP),
    ("invitation_schema_version", STARTUP),
    ("library_annotations_schema_version", STARTUP),
    ("migrate_additive_schema_step", STARTUP),
    ("migrate_playback_inputs", STARTUP),
    ("migrate_schema", STARTUP),
    ("playback_handoffs_schema_version", STARTUP),
    ("playback_input_keys_shape", STARTUP),
    ("pointer_desired_revision_column_present", STARTUP),
    ("read_lineage_snapshot", STARTUP),
    ("refuse_ambiguous_sharing_lineage", STARTUP),
    ("remote_schema_current", STARTUP),
    ("scan_request_v2_schema_version", STARTUP),
    ("schema_migration_transaction", STARTUP),
    ("seal_legacy", STARTUP),
    ("settle_migration_attempt", STARTUP),
    ("shared_caption_schema_version", STARTUP),
    ("source_arbitration_shape", STARTUP),
    ("subtitle_managed_schema_version", STARTUP),
    ("subtitle_text_translation_schema_version", STARTUP),
    ("subtitle_translation_schema_version", STARTUP),
    ("verify_compatibility", STARTUP),
    // SQLite import and its verification: an operator command, never a request.
    ("import_instance_setting", IMPORT),
    ("import_sqlite_backup_inner", IMPORT),
    ("insert_import_chunk", IMPORT),
    ("target_count", IMPORT),
    ("target_digest", IMPORT),
    ("verify_empty_import_target", IMPORT),
    ("verify_target_foreign_keys", IMPORT),
    // Validation-only hooks behind `cluster-read-cost-validation` and tests.
    ("validation_duplicate_instance_id_transaction", VALIDATION),
    ("validation_lineage_union_fingerprint", VALIDATION),
    ("validation_reset_contract_state", VALIDATION),
    ("validation_set_schema_lineage", VALIDATION),
    // The one layout probe every guarded session statement re-reads while the
    // cached layout is not the permanent v82 one. Named on purpose, so a fleet
    // that has installed v82 shows none and one that has not shows how many.
    (
        "session_layout",
        "the session-layout marker and guard reads, one bounded label",
    ),
    // The queue bridge is generic over `QueueSql`, shared by both stores and
    // the formally pinned background-jobs code; its create-path writes are
    // named in the per-shape pin. Deferred to D3b.
    ("queue_sql", "the queue bridge's statement path (D3b)"),
    (
        "queue_transaction",
        "the queue bridge's transaction path (D3b)",
    ),
    // The live-TV resource ledger's own two operations behind its private
    // `Backend` trait, which is not a `Store`.
    ("read_ledger", "live-TV resource ledger `Backend` read"),
    ("commit_ledger", "live-TV resource ledger `Backend` commit"),
    // The `bounded_replica_reads` switch read every bounded read's gate makes:
    // the gate itself, not a Store method's read.
    (
        "local_bounded_reads_enabled",
        "the bounded-replica switch read behind every gate",
    ),
    // Operator dumps and digests, not request reads.
    ("local_auth_dump", "operator dump of the auth tables"),
    ("authoritative_dump", "operator catalog dump"),
    ("local_catalog_digest", "operator catalog digest"),
    (
        "local_durable_digest",
        "operator digest of the durable tables",
    ),
    // The sharing `Backend` trait is shared with the SQLite store and its
    // 458 call sites live in the sharing modules, none on the playback path:
    // its read and transaction name themselves. Forwarding a Store method
    // through that trait is deferred with the rest of sharing attribution.
    (
        "sharing_read",
        "sharing `Backend` read, shared with SQLite; not a playback path",
    ),
    (
        "sharing_txn",
        "sharing `Backend` transaction, shared with SQLite; not a playback path",
    ),
    // Throttled last-seen writes the auth lookups trigger: their own
    // operation, one caller each, better named than folded into the lookup.
    (
        "refresh_api_key_activity",
        "throttled last-seen write after an API-key lookup",
    ),
    (
        "refresh_token_activity_if_due",
        "throttled last-seen write after a token lookup",
    ),
];
const STARTUP: &str =
    "startup, schema migration or compatibility probe, outside request accounting";
const IMPORT: &str = "SQLite import and verification, an operator command";
const VALIDATION: &str = "validation hook behind a test or cost-validation feature";

/// [`production`], also without a `#[cfg(test)]` helper module of any name.
fn label_production(source: &str) -> &str {
    production(source)
        .split("\n#[cfg(test)]\nmod ")
        .next()
        .unwrap_or(source)
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The text after the bracket that closes the one `text` opens.
fn skip_balanced(text: &str, open: char, close: char) -> &str {
    let mut depth = 0_usize;
    for (index, character) in text.char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth -= 1;
            if depth == 0 {
                return &text[index + character.len_utf8()..];
            }
        }
    }
    ""
}

/// Every function named in `sources` whose first parameter, after any
/// receiver, is `method: &'static str`: the functions that take a label.
fn label_taking_fns(sources: &[&str]) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for source in sources {
        let text = label_production(source);
        for (at, _) in text.match_indices("fn ") {
            if at > 0 && is_ident_byte(text.as_bytes()[at - 1]) {
                continue;
            }
            let rest = &text[at + 3..];
            let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
            let Some(end) = end.filter(|end| *end > 0) else {
                continue;
            };
            let mut after = rest[end..].trim_start();
            if after.starts_with('<') {
                after = skip_balanced(after, '<', '>').trim_start();
            }
            let Some(mut params) = after.strip_prefix('(').map(str::trim_start) else {
                continue;
            };
            for receiver in ["&mut self", "&self", "self"] {
                if let Some(next) = params.strip_prefix(receiver) {
                    if let Some(next) = next.trim_start().strip_prefix(',') {
                        params = next.trim_start();
                    }
                    break;
                }
            }
            if params.starts_with("method: &'static str") {
                names.insert(rest[..end].to_owned());
            }
        }
    }
    names
}

/// How a call to a label-taking function spells its label.
#[derive(Debug, PartialEq, Eq)]
enum LabelArg {
    /// The caller's own `method`, forwarded.
    Forwarded,
    /// A string literal.
    Literal(String),
}

/// Every call to one of `fns` in `source`'s production text, as `(line,
/// function, label argument)`. A call on the raw vendor client (`inner()`)
/// is the consistent-read rule's, not this one's; a bare array first argument
/// is a `rusqlite` call of the same name.
fn label_calls(
    source: &str,
    fns: &std::collections::BTreeSet<String>,
) -> Vec<(usize, String, LabelArg)> {
    let text = label_production(source);
    let mut calls = Vec::new();
    for name in fns {
        for (at, _) in text.match_indices(name.as_str()) {
            let before = &text[..at];
            let after_name = &text[at + name.len()..];
            if before
                .as_bytes()
                .last()
                .is_some_and(|byte| is_ident_byte(*byte))
                || after_name
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| is_ident_byte(*byte))
                || before.trim_end().ends_with("fn")
                || before.trim_end().ends_with("inner()")
            {
                continue;
            }
            let line_start = before.rfind('\n').map_or(0, |index| index + 1);
            if text[line_start..at].trim_start().starts_with("//") {
                continue;
            }
            let mut rest = after_name;
            if rest.starts_with("::<") {
                rest = skip_balanced(&rest[2..], '<', '>');
            }
            let Some(arguments) = rest.strip_prefix('(') else {
                continue;
            };
            let first = arguments.trim_start();
            // Anything else as the first argument is the vendor client's own
            // method of that name (`client.batch(SCHEMA)`, `rusqlite`'s
            // `query_map([], ..)`): a `TimedClient` call cannot spell it, since
            // its first parameter is the `&'static str` label. The raw
            // consistent reads are the census rule's, pinned in
            // `RAW_CLIENT_SITES`.
            let argument = if let Some(literal) = first.strip_prefix('"') {
                match literal.split_once("\",") {
                    Some((label, _)) if !label.is_empty() && label.bytes().all(is_ident_byte) => {
                        LabelArg::Literal(label.to_owned())
                    }
                    _ => continue,
                }
            } else if first
                .strip_prefix("method")
                .is_some_and(|tail| tail.trim_start().starts_with([',', ')']))
            {
                LabelArg::Forwarded
            } else {
                continue;
            };
            calls.push((before.matches('\n').count() + 1, name.clone(), argument));
        }
    }
    calls.sort_by_key(|(line, _, _)| *line);
    calls
}

/// The names declared by `Store` traits: every function inside a `trait`.
fn store_method_names(sources: &[&str]) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for source in sources {
        for (at, _) in production(source).match_indices("trait ") {
            let text = production(source);
            if at > 0 && is_ident_byte(text.as_bytes()[at - 1]) {
                continue;
            }
            let Some(open) = text[at..].find('{').map(|index| at + index) else {
                continue;
            };
            // `QueueSql` and the like are not Stores: only `...Store` traits
            // name Store methods.
            let declared = text[at + "trait ".len()..open].trim_start();
            let name_end = declared
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(declared.len());
            if !declared[..name_end].ends_with("Store") {
                continue;
            }
            let body = &text[open..];
            let closed = skip_balanced(body, '{', '}');
            let body = &body[..body.len() - closed.len()];
            for (index, _) in body.match_indices("fn ") {
                let tail = &body[index + 3..];
                let end = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(tail.len());
                names.insert(tail[..end].to_owned());
            }
        }
    }
    names
}

/// Labels in `sources` that name neither a Store method, nor the local twin
/// of one, nor an allow-listed function; and calls that spell no label.
fn label_failures(
    sources: &[(&str, &str)],
    methods: &std::collections::BTreeSet<String>,
    allowed: &[(&str, &str)],
) -> Vec<String> {
    let texts = sources.iter().map(|(_, text)| *text).collect::<Vec<_>>();
    let fns = label_taking_fns(&texts);
    let mut failures = Vec::new();
    for (name, source) in sources {
        for (line, function, argument) in label_calls(source, &fns) {
            match argument {
                LabelArg::Forwarded => {}
                LabelArg::Literal(label) => {
                    let twin = label
                        .strip_prefix("local_")
                        .is_some_and(|method| methods.contains(method));
                    if !methods.contains(&label)
                        && !twin
                        && !allowed.iter().any(|(entry, _)| *entry == label)
                    {
                        failures.push(format!(
                            "{name}:{line}: `{function}` is labelled \"{label}\", which is not a Store method"
                        ));
                    }
                }
            }
        }
    }
    failures
}

/// D3a review S1: a label names the `Store` method that issued the operation,
/// not the helper that ran it. A helper takes `method` and forwards it, so the
/// fleet's `by_method` reading resolves to methods; only the functions listed
/// in [`NOT_A_STORE_METHOD`], each with its reason, are named otherwise.
#[test]
fn every_label_names_a_store_method() {
    let methods = store_method_names(STORE_TRAIT_SOURCES);
    assert!(
        methods.len() > 500,
        "the Store trait scan found {} methods",
        methods.len()
    );
    let failures = label_failures(STORE_SOURCES, &methods, NOT_A_STORE_METHOD);
    assert!(
        failures.is_empty(),
        "labels that are not Store methods:\n{}",
        failures.join("\n")
    );
}

/// The allow-list does not rot: each entry is used, none is a Store method
/// (it would not need listing), and none is listed twice.
#[test]
fn the_label_allow_list_has_only_live_entries() {
    let methods = store_method_names(STORE_TRAIT_SOURCES);
    let texts = STORE_SOURCES
        .iter()
        .map(|(_, text)| *text)
        .collect::<Vec<_>>();
    let fns = label_taking_fns(&texts);
    let used = STORE_SOURCES
        .iter()
        .flat_map(|(_, source)| label_calls(source, &fns))
        .filter_map(|(_, _, argument)| match argument {
            LabelArg::Literal(label) => Some(label),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut seen = std::collections::BTreeSet::new();
    for (entry, reason) in NOT_A_STORE_METHOD {
        assert!(!reason.is_empty(), "{entry} has no reason");
        assert!(seen.insert(*entry), "{entry} is listed twice");
        assert!(
            !methods.contains(*entry),
            "{entry} is a Store method and needs no entry"
        );
        assert!(
            used.contains(*entry),
            "{entry} is allow-listed but no call labels it; remove the entry"
        );
    }
}

#[test]
fn the_label_rule_refuses_a_helper_name_and_admits_store_methods() {
    let source = "\
impl S {\n\
    async fn helper(&self, method: &'static str, sql: &'static str) {\n\
        self.client().execute(method, sql, p).await;\n\
    }\n\
    async fn get_user(&self) {\n\
        self.helper(\"get_user\", SQL).await;\n\
        self.helper(\"local_get_user\", SQL).await;\n\
        self.helper(\"helper\", SQL).await;\n\
        self.helper(\"get_user\", SQL).await;\n\
        self.inner().helper(sql, p);\n\
    }\n\
}\n";
    let methods = ["get_user"]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let failures = label_failures(&[("fixture.rs", source)], &methods, &[]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("labelled \"helper\""), "{failures:?}");
    // An allow-listed function is admitted and named.
    assert!(
        label_failures(&[("fixture.rs", source)], &methods, &[("helper", "why")])
            .iter()
            .all(|failure| !failure.contains("labelled")),
    );
}
