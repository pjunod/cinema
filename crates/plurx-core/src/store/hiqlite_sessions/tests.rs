use super::{
    abort_statements, prepare_statements, MEDIA_SESSIONS_SCHEMA, MEDIA_SESSIONS_V10_SCHEMA,
    MEDIA_SESSION_PUBLICATION_FENCE_MIGRATION, MEDIA_SESSION_TERMINAL_REASON_MIGRATION,
};

const SOURCE: &str = include_str!("../hiqlite_sessions.rs");

fn method_source(method: &str) -> &'static str {
    let declaration = format!("    async fn {method}(");
    let start = SOURCE
        .find(&declaration)
        .unwrap_or_else(|| panic!("missing {declaration} in hiqlite_sessions.rs"));
    let rest = &SOURCE[start..];
    let end = rest[1..]
        .find("\n    async fn ")
        .map_or(rest.len(), |offset| offset + 1);
    &rest[..end]
}

fn statement_test_preparation() -> crate::domain::MediaSessionPreparation {
    crate::domain::MediaSessionPreparation {
        quality_cancellation_key: None,
        expected_desired_revision: None,
        incarnation_id: "00000000-0000-4000-8000-000000000002".to_owned(),
        session_id: "session".to_owned(),
        principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 },
        playback_id: "playback".to_owned(),
        expected_predecessor_incarnation_id: "00000000-0000-4000-8000-000000000001".to_owned(),
        expected_predecessor_owner_node_id: "owner".to_owned(),
        expected_predecessor_owner_epoch: 1,
        request_fingerprint: "a".repeat(64),
        owner_node_id: "owner".to_owned(),
        recipe_json: "{}".to_owned(),
        response_json: "{}".to_owned(),
        media_origin_ms: 0,
        now_ms: 1,
        deadline_ms: 2,
    }
}

// The placeholder-order census this module used to carry alone now runs
// over every replicated slice: `store::placeholder_census`. Scoping it to
// one file is what let the same mistake land in
// `hiqlite_fragment_index_cluster.rs` and `hiqlite_publication.rs` six
// weeks later.

/// Ending a session must act on the owner the row has at commit time, not
/// on the snapshot the pre-transaction read returned.
///
/// The read that drives `end_media_session` is outside its transaction, so
/// a takeover can commit between the two. If the dependent statements are
/// driven by Rust-side `route.owner_node_id` / `route.owner_epoch`, the
/// end clamps a lease the dead node no longer holds and leaves the
/// successor's pin behind. That interleaving is not reproducible through
/// the public store API, so the property is asserted where it lives: the
/// statements read owner and fence back out of `media_sessions`.
#[test]
fn ending_a_session_reads_its_owner_inside_the_transaction() {
    let source = method_source("end_media_session");
    assert!(
        source.contains("media_sessions.owner_node_id = job_leases.owner_node_id")
            && source.contains("media_sessions.owner_epoch = job_leases.fence"),
        "the lease clamp must correlate against the row, not a pre-read snapshot"
    );
    assert!(
        !source.contains("route.owner_node_id.as_str()") && !source.contains("route.owner_epoch"),
        "no statement in this transaction may bind the pre-read owner or fence"
    );
    assert!(
        !source.contains("consumer_epoch = $"),
        "the pin delete covers every epoch of the ended incarnation"
    );
    assert!(
        source.contains("Ok(route_by(self, \"incarnation_id\", &route.incarnation_id)"),
        "the returned route must be re-read after the transaction commits"
    );
}

/// Reproduce the ordering which the backend-neutral sequential contract
/// cannot force: caller A reads the predecessor pointer, caller B commits
/// the exact activation and renews it, then caller A finally enters its
/// replicated transaction with the stale activation lease.
///
/// The transaction must observe B's current pointer and roll A's proposal
/// back. In particular it may not rewrite the renewed successor/job lease
/// merely because those later statements see B's already-current pointer.
/// The pointer CAS therefore exposes a row and the next statement consumes
/// it: a zero-row CAS is a statement-output error, which Hiqlite rolls back
/// before the exact durable replay projection runs.
#[test]
fn preparation_commit_disambiguates_replay_from_total_loss() {
    let source = method_source("commit_media_session_preparation");
    assert!(
        source.contains("RETURNING current_incarnation_id")
            && source.contains("Param::StmtOutputNamed(0, \"current_incarnation_id\".into())")
            && source.contains("StmtIndex(1) does not have observable row output"),
        "a losing pointer CAS must roll back every dependent write before replay classification"
    );
    assert!(
        source.contains("pointer.as_deref() != Some(staged.staged_incarnation_id.as_str())"),
        "and the exact post-commit pointer projection is what settles it"
    );
    assert_eq!(
        source.matches("current_incarnation_id = $5").count(),
        1,
        "the pointer advance fences on the recorded predecessor, never on \
             whatever the pointer happens to name"
    );
    assert!(
            !source.contains("SELECT current_incarnation_id FROM media_playback_pointers\n                      WHERE user_id = $4 AND playback_id = $5)"),
            "the predecessor's retirement must name an exact incarnation, not \
             a pointer subquery — that is the §2.4 divergence"
        );
    assert!(
        source.contains("abort_media_session_preparation("),
        "losing the pointer CAS aborts the staged successor rather than \
             leaving the ledger row to lock the playback out of preparation"
    );
}

/// A preparation must not run the supersession reap or move the pointer.
///
/// Acceptance 3, as a source guard, because the runnable form of it needs
/// a three-voter cluster and does not run in the fast loop.
#[test]
fn preparation_neither_reaps_nor_repoints() {
    let method = method_source("prepare_media_session");
    let normalized_method = method.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized_method.contains("let statements = prepare_statements(preparation, layout)?;")
            && method.matches("let statements =").count() == 1
            && method.matches(".txn(").count() == 1
            && method.matches(".txn(statements)").count() == 1
            && !method.contains("statements.push(")
            && !method.contains("statements.extend(")
            && !method.contains("statements.insert(")
            && !method.contains("statements.pop(")
            && !method.contains(".execute")
            && !method.contains("\"INSERT ")
            && !method.contains("\"UPDATE ")
            && !method.contains("\"DELETE "),
        "the public preparation path must submit the shared statement vector unchanged"
    );
    let preparation = statement_test_preparation();
    let statements = prepare_statements(
        &preparation,
        super::LocalSessionSql {
            rebuilt: false,
            template: false,
        },
    )
    .expect("local preparation statements");
    assert_eq!(
        statements.len(),
        4,
        "preparation is exactly lease, session, ledger, then the quality \
             cancellation owner binding"
    );
    assert!(
        statements[3]
            .0
            .contains("INSERT OR IGNORE INTO quality_preparation_owners"),
        "the fourth statement binds the staged successor to its quality \
             cancellation key in the same transaction"
    );
    assert!(
        statements[0].0.contains("INSERT INTO job_leases")
            && statements[1].0.contains("INSERT INTO media_sessions")
            && statements[2]
                .0
                .contains("INSERT INTO media_session_preparations"),
        "preparation statement order is lease, session, then ledger"
    );
    let statement_sql = statements
        .iter()
        .map(|(sql, _)| sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !method.contains("terminal_reason = 'superseded'")
            && !statement_sql.contains("terminal_reason = 'superseded'"),
        "prepare must not run the supersession reap"
    );
    let normalized_statements = statement_sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !normalized_method.contains("UPDATE media_playback_pointers")
            && !normalized_method.contains("INSERT INTO media_playback_pointers")
            && !normalized_method.contains("DELETE FROM media_playback_pointers")
            && !normalized_statements.contains("UPDATE media_playback_pointers")
            && !normalized_statements.contains("INSERT INTO media_playback_pointers")
            && !normalized_statements.contains("DELETE FROM media_playback_pointers"),
        "prepare must not write the pointer — a staged successor is \
             invisible to the current-session lookup precisely because it \
             holds none"
    );
    let lease_sql = statements[0]
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let session_sql = statements[1]
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
            lease_sql.contains(
                "NOT EXISTS (SELECT 1 FROM media_session_preparations WHERE user_id = $5 AND playback_id = $6)"
            ) && session_sql.contains(
                "NOT EXISTS (SELECT 1 FROM media_session_preparations WHERE user_id = $3 AND playback_id = $4)"
            ),
            "both lease and session admission enforce one staged successor per \
             playback because a replicated transaction cannot read and branch"
        );
    // The ordered identity assertion above also proves that a staged
    // successor takes its own session lease, without which it could never
    // be renewed or taken over once it commits.
}

/// Every write an abort performs is gated on the ledger row.
///
/// The incarnation id alone names any session in the database, so an
/// ungated `WHERE incarnation_id = ?` ends a stranger's live stream.
#[test]
fn preparation_abort_is_ledger_gated_in_every_statement() {
    let method = method_source("abort_media_session_preparation");
    let normalized_method = method.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
            normalized_method.contains(
                "let statements = abort_statements( user_id, playback_id, &request.staged_incarnation_id, &request.expected_predecessor_owner_node_id, request.expected_predecessor_owner_epoch, request.now_ms, layout, );"
            ) && method.matches("let statements =").count() == 1
                && method.matches(".txn(").count() == 1
                && method.matches(".txn(statements)").count() == 1
                && !method.contains("statements.push(")
                && !method.contains("statements.extend(")
                && !method.contains("statements.insert(")
                && !method.contains("statements.pop(")
                && !method.contains(".execute")
                && !method.contains("\"INSERT ")
                && !method.contains("\"UPDATE ")
                && !method.contains("\"DELETE "),
            "the public abort path must submit the shared statement vector unchanged"
        );
    let statements = abort_statements(
        1,
        "playback",
        "00000000-0000-4000-8000-000000000002",
        "owner",
        1,
        1,
        super::LocalSessionSql {
            rebuilt: false,
            template: false,
        },
    );
    assert_eq!(
        statements.len(),
        4,
        "abort is exactly retirement, pin delete, lease clamp, then ledger delete"
    );
    let retirement = &statements[0].0;
    let pin_delete = &statements[1].0;
    let lease_clamp = &statements[2].0;
    let ledger_delete = &statements[3].0;
    let normalized_retirement = retirement.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized_pin_delete = pin_delete.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized_lease_clamp = lease_clamp.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized_ledger_delete = ledger_delete
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        retirement.contains("UPDATE media_sessions")
            && pin_delete.contains("DELETE FROM cache_consumer_pins")
            && lease_clamp.contains("UPDATE job_leases")
            && ledger_delete.contains("DELETE FROM media_session_preparations"),
        "the abort statement order is part of its transaction contract"
    );
    assert!(
            normalized_retirement.contains(
                "EXISTS (SELECT 1 FROM media_session_preparations WHERE user_id = $3 AND playback_id = $4 AND staged_incarnation_id = $2)"
            ),
            "retirement is gated on the exact ledger identity"
        );
    assert!(
            normalized_pin_delete.contains(
                "EXISTS (SELECT 1 FROM media_sessions WHERE incarnation_id = $1 AND state = 'ended' AND updated_at_ms = $2 AND user_id = $3)"
            ) && normalized_pin_delete.contains(
                "EXISTS (SELECT 1 FROM media_session_preparations WHERE user_id = $3 AND playback_id = $4 AND staged_incarnation_id = $1)"
            ),
            "pin deletion is gated on this retirement and exact ledger identity"
        );
    assert!(
            normalized_lease_clamp.contains(
                "EXISTS (SELECT 1 FROM media_sessions WHERE incarnation_id = $3 AND state = 'ended' AND updated_at_ms = $1 AND user_id = $4)"
            ) && normalized_lease_clamp.contains(
                "EXISTS (SELECT 1 FROM media_session_preparations WHERE user_id = $4 AND playback_id = $5 AND staged_incarnation_id = $3)"
            ),
            "the pin delete and the lease clamp also chain on that retirement \
             having fired at this instant — `now_ms` is the caller's, so the \
             timestamp alone is not proof, and the ledger EXISTS alone does \
             not say the work is this abort's"
        );
    assert!(
        normalized_ledger_delete
            .contains("WHERE user_id = $1 AND playback_id = $2 AND staged_incarnation_id = $3")
            && !ledger_delete.contains("EXISTS (SELECT 1 FROM media_session_preparations"),
        "only the final exact-identity ledger DELETE is not ledger-existence gated"
    );
}

#[test]
fn activation_losing_pointer_read_race_is_a_read_only_replay() {
    let wrapper = method_source("activate_media_session");
    assert!(wrapper.contains("activate_with_authority(self, activation, None, None).await"));
    let source = SOURCE
        .split_once("async fn activate_with_authority(")
        .expect("activation helper")
        .1
        .split_once("\n#[async_trait]")
        .expect("activation helper boundary")
        .0;
    assert_eq!(
            source.matches("current_incarnation_id = $8)").count(),
            2,
            "both the job-lease insert and conflict update must be suppressed by the transaction-time pointer"
        );
    assert!(
            source.contains("let owner_3 = layout.equals(3);") && source.contains(
                "AND NOT EXISTS (\n                      SELECT 1 FROM media_playback_pointers\n                       WHERE {owner_3} AND playback_id = $4\n                         AND current_incarnation_id = $1)"
            ),
            "the session insert/upsert must be suppressed after the exact activation wins"
        );
    assert!(
            source.contains(
                "media_playback_pointers.current_incarnation_id\n                        != excluded.current_incarnation_id"
            ),
            "an already-current pointer must not rewrite its update timestamp"
        );
    assert!(
        source.contains("OR (state = 'resolved' AND response_json = $9))"),
        "an exact resolved request must remain eligible for read-only replay"
    );
    assert!(
        !source.contains("UPDATE media_session_requests")
            && !source.contains("INSERT INTO media_session_requests")
            && !source.contains("DELETE FROM media_session_requests"),
        "activation may validate an existing request but must not mutate it"
    );
    assert!(
        source.contains("changed.iter().all(|affected| *affected == 0)"),
        "the replicated transaction must recognize the read-only race result"
    );
    assert!(
        source.contains("committed_pointer.as_deref() != Some(activation.incarnation_id.as_str())"),
        "all-zero transactions must prove the exact pointer after commit"
    );
    assert_eq!(
        source
            .matches(".filter(|(route, _)| activation_route_matches(route, activation))")
            .count(),
        1,
        "all-zero transactions must also prove the immutable activation identity \
             on the post-commit route-and-pointer projection"
    );
}

#[test]
fn direct_v9_upgrade_uses_the_frozen_v10_session_shape() {
    assert!(!MEDIA_SESSIONS_V10_SCHEMA.contains("terminal_reason"));
    assert!(!MEDIA_SESSIONS_V10_SCHEMA.contains("publication_ready_at_ms"));
    assert!(MEDIA_SESSIONS_SCHEMA.contains("terminal_reason"));
    assert!(MEDIA_SESSIONS_SCHEMA.contains("publication_ready_at_ms"));
    assert!(MEDIA_SESSION_TERMINAL_REASON_MIGRATION.contains("terminal_reason"));
    assert!(MEDIA_SESSION_PUBLICATION_FENCE_MIGRATION.contains("publication_ready_at_ms"));
}
