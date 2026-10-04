//! Crash recovery for orphaned B RemoteSource routes.
//!
//! A receiver owner that crashed (or whose in-process retirement stalled)
//! leaves its route, relay binding, job lease and request behind; nothing
//! else ends them. This owner finds such routes, takes one over through the
//! exclusive epoch-fencing Store claim, and retires it with the same
//! machinery the live owner uses. It never adopts the producer: no Start,
//! registry actor, attach, publish, renewal or delivery. Lease expiry, row
//! absence, a lost claim or a Source refusal never delete anything; only a
//! confirmed Source End (or the claim-fenced durable no-send marker) does.
use super::*;
use plurx_core::sharing_receiver_retirement::{
    ClaimedReceiverOrphan, ReceiverOrphan, ReceiverOrphanClaimOutcome, ReceiverOrphanDispatch,
};

/// One look per period. The inventory is a bounded keyset page; a pass over
/// many orphans spans several periods instead of one long burst.
const RECOVERY_TICK: Duration = Duration::from_secs(30);
const RECOVERY_BATCH: usize = 8;
/// Concurrent retirements per page; each holds one pinned cleanup dial.
const RECOVERY_PARALLEL: usize = 2;
/// Reported stranded rows are bounded; the oldest observation yields.
const STRANDED_MAX: usize = 32;

/// What one recovery attempt ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrphanOutcome {
    /// Confirmed exact retirement (Applied or Replay).
    Retired,
    /// Recovery cannot retire this row; it is kept and reported.
    Stranded(&'static str),
    /// Another owner, renewal or retirement changed the row first.
    Lost,
    /// Not attempted now (live local owner, drain, Store unavailable,
    /// refused claim); a later look decides again.
    Deferred(&'static str),
}

/// End material opened from the durable capsule, or the no-send proof.
enum EndPlan {
    Source(Box<SourceEnd>),
    NeverDispatched,
}
struct SourceEnd {
    credential: plurx_core::secrets::Secret,
    viewer_hash: String,
    endpoint: plurx_core::sharing::Endpoint,
    lineage: Option<crate::sharing_client::SourcePeerLineage>,
    session: crate::sharing_client::SourcePeerSession,
}

/// Facts opened from an attached binding's sealed upstream capsule.
struct OpenedUpstream {
    credential: plurx_core::secrets::Secret,
    viewer_hash: String,
    endpoint: plurx_core::sharing::Endpoint,
    lineage: crate::sharing_client::SourcePeerLineage,
}

/// Open the attached capsule under this server/import AAD and require its
/// Source identities to equal the durable binding columns exactly.
fn open_upstream_capsule(
    key: &plurx_core::secrets::CredentialKey,
    server: Uuid,
    intent: &ReceiverSessionIntent,
    binding: &ReceiverSourceBinding,
) -> Result<OpenedUpstream, ()> {
    let opened = key
        .open_sharing(
            SharingSecretPurpose::Upstream,
            server,
            intent.scope.import_id,
            &binding.capability_envelope,
        )
        .map_err(|_| ())?;
    let capsule: UpstreamCapsule = serde_json::from_str(opened.expose()).map_err(|_| ())?;
    let recipe = &intent.recipe;
    if capsule.version != 1
        || capsule.reference != recipe.reference
        || binding.reference != recipe.reference
        || capsule.file_id != recipe.file_id
        || capsule.file_revision != recipe.file_revision
        || capsule.source_request_id != recipe.source_request_id
        || capsule.source_session_id != binding.source_session_id
        || capsule.source_incarnation_id != binding.source_incarnation_id
    {
        return Err(());
    }
    let lineage = crate::sharing_client::SourcePeerLineage::from_capsule(
        capsule.source_incarnation_id,
        capsule.source_session_id,
        capsule.source_owner_epoch,
    )
    .map_err(|_| ())?;
    Ok(OpenedUpstream {
        credential: capsule.credential,
        viewer_hash: capsule.viewer_hash,
        endpoint: capsule.endpoint,
        lineage,
    })
}

/// Open the dispatch capsule recorded before the first Start byte.
fn open_dispatch_capsule(
    key: &plurx_core::secrets::CredentialKey,
    server: Uuid,
    intent: &ReceiverSessionIntent,
    sealed: &plurx_core::secrets::SealedSecret,
) -> Result<
    (
        plurx_core::secrets::Secret,
        String,
        plurx_core::sharing::Endpoint,
    ),
    (),
> {
    let opened = key
        .open_sharing(
            SharingSecretPurpose::Upstream,
            server,
            intent.scope.import_id,
            sealed,
        )
        .map_err(|_| ())?;
    let capsule: DispatchCapsule = serde_json::from_str(opened.expose()).map_err(|_| ())?;
    let recipe = &intent.recipe;
    if capsule.version != 1
        || capsule.kind != DISPATCH_CAPSULE_KIND
        || capsule.reference != recipe.reference
        || capsule.file_id != recipe.file_id
        || capsule.file_revision != recipe.file_revision
        || capsule.source_request_id != recipe.source_request_id
    {
        return Err(());
    }
    Ok((capsule.credential, capsule.viewer_hash, capsule.endpoint))
}

/// The exact Source session the dispatched Start named, rebuilt from the
/// durable recipe through the same canonical wrapper ingress used.
fn source_session(
    intent: &ReceiverSessionIntent,
) -> Result<crate::sharing_client::SourcePeerSession, OrphanOutcome> {
    let wrapper = crate::sharing::receiver_source_wrapper(&intent.recipe)
        .map_err(|_| OrphanOutcome::Stranded("unrecoverable_request"))?;
    let reference = crate::sharing::receiver_source_request(intent, &wrapper)
        .map_err(|_| OrphanOutcome::Stranded("unrecoverable_request"))?;
    crate::sharing_client::SourcePeerSession::new(reference, wrapper.as_bytes())
        .map_err(|_| OrphanOutcome::Stranded("unrecoverable_request"))
}

/// Everything needed to send End is opened before claiming, so a node that
/// cannot open the capsule (wrong key) never takes a route it cannot settle.
async fn end_plan(state: &AppState, orphan: &ReceiverOrphan) -> Result<EndPlan, OrphanOutcome> {
    let intent = orphan.intent();
    match (orphan.binding(), orphan.dispatch()) {
        (None, ReceiverOrphanDispatch::NotDispatched) => Ok(EndPlan::NeverDispatched),
        (None, ReceiverOrphanDispatch::Unknown) => Err(OrphanOutcome::Stranded("dispatch_unknown")),
        (binding, dispatch) => {
            let local = state
                .store
                .sharing_identity(clock_ms())
                .await
                .map_err(|_| OrphanOutcome::Deferred("identity_unavailable"))?;
            let session = source_session(intent)?;
            if let Some(binding) = binding {
                let opened =
                    open_upstream_capsule(&state.sharing.key, local.server_id, intent, binding)
                        .map_err(|_| OrphanOutcome::Stranded("unopenable_capsule"))?;
                return Ok(EndPlan::Source(Box::new(SourceEnd {
                    credential: opened.credential,
                    viewer_hash: opened.viewer_hash,
                    endpoint: opened.endpoint,
                    lineage: Some(opened.lineage),
                    session,
                })));
            }
            let ReceiverOrphanDispatch::Sealed(sealed) = dispatch else {
                return Err(OrphanOutcome::Stranded("dispatch_unknown"));
            };
            let (credential, viewer_hash, endpoint) =
                open_dispatch_capsule(&state.sharing.key, local.server_id, intent, sealed)
                    .map_err(|_| OrphanOutcome::Stranded("unopenable_capsule"))?;
            // A lost Start: End carries no lineage; Source answers for the request.
            Ok(EndPlan::Source(Box::new(SourceEnd {
                credential,
                viewer_hash,
                endpoint,
                lineage: None,
                session,
            })))
        }
    }
}

/// Removes its incarnation from the visible in-flight set on every exit.
struct InFlight<'a>(&'a AppState, Uuid);
impl<'a> InFlight<'a> {
    fn enter(state: &'a AppState, incarnation: Uuid) -> Self {
        state
            .sharing
            .receiver_recovery
            .lock()
            .expect("receiver recovery status")
            .in_flight
            .push(incarnation);
        Self(state, incarnation)
    }
}
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0
            .sharing
            .receiver_recovery
            .lock()
            .expect("receiver recovery status")
            .in_flight
            .retain(|id| *id != self.1);
    }
}

fn stall_outcome(stall: RetirementStall) -> OrphanOutcome {
    match stall {
        RetirementStall::Shutdown => OrphanOutcome::Deferred("shutdown"),
        RetirementStall::Exhausted => OrphanOutcome::Stranded("budget_exhausted"),
        RetirementStall::Refused => OrphanOutcome::Stranded("retirement_refused"),
        RetirementStall::Unjoined => OrphanOutcome::Stranded("unjoined"),
    }
}

/// One orphan: decide, claim, then retire through the exact Store witness.
async fn recover_orphan(
    state: &AppState,
    orphan: ReceiverOrphan,
    shutdown: &tokio_util::sync::CancellationToken,
) -> OrphanOutcome {
    if shutdown.is_cancelled() {
        return OrphanOutcome::Deferred("shutdown");
    }
    if let Some(reason) = orphan.stranded() {
        return OrphanOutcome::Stranded(reason.as_str());
    }
    let incarnation = orphan.owner().incarnation_id;
    if state.sharing.receiver_starts.holds(incarnation) {
        return OrphanOutcome::Deferred("live_local");
    }
    let plan = match end_plan(state, &orphan).await {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    let _flight = InFlight::enter(state, incarnation);
    let claimed = match state
        .store
        .claim_orphaned_receiver_session(&orphan, &state.node_id)
        .await
    {
        Ok(ReceiverOrphanClaimOutcome::Claimed(claimed)) => claimed,
        Ok(ReceiverOrphanClaimOutcome::Lost) => return OrphanOutcome::Lost,
        Ok(ReceiverOrphanClaimOutcome::Refused) => return OrphanOutcome::Deferred("claim_refused"),
        Err(_) => return OrphanOutcome::Deferred("store_unavailable"),
    };
    retire_claimed(state, *claimed, plan, shutdown).await
}

async fn retire_claimed(
    state: &AppState,
    claimed: ClaimedReceiverOrphan,
    plan: EndPlan,
    shutdown: &tokio_util::sync::CancellationToken,
) -> OrphanOutcome {
    let orphan = claimed.orphan();
    let owner = orphan.owner().clone();
    let mut budget = RetirementBudget::new(shutdown.clone());
    let (disposition, confirmation, source) = match plan {
        EndPlan::NeverDispatched => {
            // The claim advanced the epoch, so the dead owner's dispatch
            // record (which needs its own epoch) can never be written: the
            // committed claim over `none` is the durable no-send proof.
            let identity = match serde_json::to_vec(&serde_json::json!({
                "recipe":orphan.intent().recipe,"incarnation":owner.incarnation_id,
                "session":owner.session_id,"node":owner.owner_node_id,"epoch":owner.owner_epoch,
                "request":owner.request_id,
            })) {
                Ok(identity) => identity,
                Err(_) => return OrphanOutcome::Stranded("unrecoverable_request"),
            };
            let mut digest = Sha256::new();
            digest.update(b"plurx.receiver.claimed-orphan-never-dispatched.v1\0");
            digest.update(identity);
            (
                ReceiverRetirementDisposition::NeverDispatched,
                format!("{:x}", digest.finalize()),
                None,
            )
        }
        EndPlan::Source(end) => loop {
            let SourceEnd {
                credential,
                viewer_hash,
                endpoint,
                lineage,
                session,
            } = end.as_ref();
            let attempt = async {
                let mut connection =
                    crate::sharing_client::CleanupPeerConnection::connect(&state.sharing, endpoint)
                        .await?;
                connection
                    .end(credential, viewer_hash, session, lineage.as_ref())
                    .await
            }
            .await;
            match attempt {
                Ok(receipt) => match receipt.retirement_confirmation(session) {
                    Ok(confirmation) => {
                        break (
                            ReceiverRetirementDisposition::SourceSettled,
                            confirmation,
                            Some(Arc::new(receipt)),
                        )
                    }
                    Err(_) => return OrphanOutcome::Stranded("invalid_end_receipt"),
                },
                Err(error) => match RetirementStep::from_source_end(error) {
                    RetirementStep::Refused => return OrphanOutcome::Stranded("source_refused"),
                    RetirementStep::Retry => {
                        if let Err(stall) = budget.retry().await {
                            return stall_outcome(stall);
                        }
                    }
                },
            }
        },
    };
    let mut witness = ConfirmedRetirement {
        intent: orphan.intent().clone(),
        owner,
        binding: orphan.binding().cloned(),
        disposition,
        reason: ReceiverRetirementReason::Replaced,
        confirmation,
        _source: source,
    };
    loop {
        match state.store.retire_receiver_session(&witness).await {
            Ok(ReceiverRetirementOutcome::Applied | ReceiverRetirementOutcome::Replay) => {
                return OrphanOutcome::Retired;
            }
            Ok(ReceiverRetirementOutcome::Refused) => {
                // Maintenance may move only the claimed route's lease while
                // node, epoch and session stay ours; anything else is final.
                match state
                    .store
                    .media_session_route_by_incarnation(&witness.owner.incarnation_id.to_string())
                    .await
                {
                    Ok(Some(route))
                        if route.session_id == witness.owner.session_id.to_string()
                            && route.owner_node_id == witness.owner.owner_node_id
                            && route.owner_epoch == witness.owner.owner_epoch =>
                    {
                        if route.lease_expires_at_ms == witness.owner.lease_expires_at_ms {
                            return OrphanOutcome::Stranded("retirement_refused");
                        }
                        witness.owner.lease_expires_at_ms = route.lease_expires_at_ms;
                    }
                    Ok(Some(_)) => return OrphanOutcome::Lost,
                    Ok(None) => return OrphanOutcome::Stranded("retirement_refused"),
                    Err(_) => {}
                }
            }
            // Commit-unknown keeps the exact witness and its End receipt.
            Err(_) => {}
        }
        if let Err(stall) = budget.retry().await {
            return stall_outcome(stall);
        }
    }
}

fn report(state: &AppState, import: Uuid, incarnation: &str, outcome: OrphanOutcome) {
    let now = clock_ms();
    let mut status = state
        .sharing
        .receiver_recovery
        .lock()
        .expect("receiver recovery status");
    match outcome {
        OrphanOutcome::Retired => {
            status.retired_total = status.retired_total.saturating_add(1);
            status.stranded.retain(|s| s.incarnation_id != incarnation);
            drop(status);
            crate::telemetry::record_receiver_orphan("retired");
            tracing::info!(target: "plurxd::sharing", import = %import, incarnation, "orphaned shared playback route retired after a confirmed Source End");
        }
        OrphanOutcome::Stranded(reason) => {
            if let Some(entry) = status
                .stranded
                .iter_mut()
                .find(|s| s.incarnation_id == incarnation)
            {
                entry.reason = reason;
                entry.observed_at_ms = now;
                return;
            }
            if status.stranded.len() >= STRANDED_MAX {
                status.stranded.remove(0);
            }
            status.stranded.push(crate::sharing::StrandedReceiver {
                incarnation_id: incarnation.to_owned(),
                reason,
                observed_at_ms: now,
            });
            drop(status);
            crate::telemetry::record_receiver_orphan("stranded");
            tracing::warn!(target: "plurxd::sharing", import = %import, incarnation, reason, "orphaned shared playback route cannot be retired; its route and binding keep their exact lineage");
        }
        OrphanOutcome::Lost => {
            drop(status);
            crate::telemetry::record_receiver_orphan("lost");
        }
        OrphanOutcome::Deferred(_) => {}
    }
}

/// One keyset page. Returns the next cursor, `None` at the end of a pass.
async fn sweep_page(
    state: &AppState,
    cursor: Option<&str>,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<Option<String>, ()> {
    use futures_util::StreamExt;
    let page = state
        .store
        .orphaned_receiver_sessions(cursor, RECOVERY_BATCH)
        .await
        .map_err(|_| ())?;
    for incarnation in &page.unreadable {
        report(
            state,
            Uuid::nil(),
            incarnation,
            OrphanOutcome::Stranded("unreadable"),
        );
    }
    futures_util::stream::iter(page.orphans)
        .map(|orphan| async move {
            let import = orphan.intent().scope.import_id;
            let incarnation = orphan.owner().incarnation_id.to_string();
            let outcome = recover_orphan(state, orphan, shutdown).await;
            report(state, import, &incarnation, outcome);
        })
        .buffer_unordered(RECOVERY_PARALLEL)
        .collect::<Vec<()>>()
        .await;
    Ok(page.next_cursor)
}

/// The node's orphaned-receiver owner, spawned beside the sharing claim
/// loop. It runs with sharing off too: recovery is cleanup. Drain ends it
/// between attempts; an attempt in flight is bounded by its own deadlines.
pub(crate) async fn receiver_recovery_loop(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut cursor: Option<String> = None;
    let mut pass_started = clock_ms();
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        if cursor.is_none() {
            pass_started = clock_ms();
        }
        let next = sweep_page(&state, cursor.as_deref(), &shutdown).await;
        {
            let mut status = state
                .sharing
                .receiver_recovery
                .lock()
                .expect("receiver recovery status");
            status.last_scan_at_ms = Some(clock_ms());
            if matches!(next, Ok(None)) {
                // A full pass re-observes every stranded row it still sees.
                status.stranded.retain(|s| s.observed_at_ms >= pass_started);
            }
        }
        // On a Store error the same page is read on the next look.
        if let Ok(next) = next {
            cursor = next;
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(RECOVERY_TICK) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::{
        sharing::SourceId,
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision,
        sharing_receiver_sessions::{ReceiverProducerKind, RemoteSourceRecipe},
        store::{
            sharing_catalogue::ReceiverCatalogueScope,
            sharing_receiver_orphans::RECEIVER_ORPHAN_GRACE_MS, SqliteStore,
        },
    };

    fn endpoint() -> plurx_core::sharing::Endpoint {
        plurx_core::sharing::Endpoint {
            ipv4: std::net::Ipv4Addr::new(100, 100, 100, 1),
            ipv6: None,
            ts_fqdn: "source.example.ts.net".into(),
            port: 4433,
            spki_sha256: "a".repeat(64),
        }
    }

    fn intent(
        label: &str,
        user: i64,
        hash: &str,
        scope: &ReceiverCatalogueScope,
    ) -> ReceiverSessionIntent {
        let reference = SharedReference {
            import_id: scope.import_id,
            server_id: scope.source_server_id,
            catalogue_epoch: scope.catalogue_epoch,
            library_id: scope.libraries[0].clone(),
            item_id: SourceId::parse("9007199254740993").expect("item"),
        };
        ReceiverSessionIntent {
            scope: scope.clone(),
            user_id: user,
            login_hash: hash.to_owned(),
            source_position_ms: 0,
            recipe: RemoteSourceRecipe {
                kind: ReceiverProducerKind::RemoteSource,
                version: 1,
                reference,
                lifecycle_generation: 1,
                file_id: SourceId::parse("0").expect("file"),
                file_revision: FileRevision::parse(&"b".repeat(64)).expect("revision"),
                source_request_id: Uuid::new_v4(),
                parent_login_hash: hash.to_owned(),
                request_json: serde_json::to_string(&serde_json::json!({
                    "playback_id":format!("player-{label}"), "request_id":format!("request-{label}"),
                    "start":30.5, "copy":true,
                }))
                .expect("complete retained request"),
            },
        }
    }

    fn scope() -> ReceiverCatalogueScope {
        ReceiverCatalogueScope {
            import_id: Uuid::new_v4(),
            source_server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            lifecycle_generation: 1,
            assignment_generation: 1,
            endpoint_generation: 1,
            claim_id: Uuid::new_v4(),
            remote_grant_id: Uuid::new_v4(),
            libraries: vec![SourceId::parse("0").expect("library")],
        }
    }

    fn upstream(
        key: &plurx_core::secrets::CredentialKey,
        server: Uuid,
        intent: &ReceiverSessionIntent,
        session: Uuid,
        incarnation: Uuid,
        epoch: u64,
    ) -> ReceiverSourceBinding {
        let credential = plurx_core::sharing::new_secret().expect("credential");
        let viewer = "d".repeat(64);
        let endpoint = endpoint();
        let capsule = serde_json::to_string(&UpstreamCapsuleOut {
            version: 1,
            reference: &intent.recipe.reference,
            file_id: &intent.recipe.file_id,
            file_revision: &intent.recipe.file_revision,
            source_request_id: intent.recipe.source_request_id,
            source_session_id: session,
            source_incarnation_id: incarnation,
            source_owner_epoch: epoch,
            viewer_hash: &viewer,
            endpoint: &endpoint,
            credential: credential.expose(),
        })
        .expect("capsule");
        ReceiverSourceBinding {
            reference: intent.recipe.reference.clone(),
            file_id: intent.recipe.file_id.clone(),
            file_revision: intent.recipe.file_revision.clone(),
            source_request_id: intent.recipe.source_request_id,
            source_session_id: session,
            source_incarnation_id: incarnation,
            capability_envelope: key
                .seal_sharing(
                    SharingSecretPurpose::Upstream,
                    server,
                    intent.scope.import_id,
                    &capsule,
                )
                .expect("sealed"),
        }
    }

    #[test]
    fn receiver_orphan_capsule_rejects_foreign_aad_and_lineage() {
        let key = plurx_core::secrets::CredentialKey::generate();
        let server = Uuid::new_v4();
        let scope = scope();
        let intent = intent("capsule", 1, &"a".repeat(64), &scope);
        let (session, incarnation) = (Uuid::new_v4(), Uuid::new_v4());
        let binding = upstream(&key, server, &intent, session, incarnation, 7);
        let opened = open_upstream_capsule(&key, server, &intent, &binding).expect("own capsule");
        assert_eq!(opened.viewer_hash, "d".repeat(64));
        assert_eq!(opened.endpoint, endpoint());
        assert!(
            opened.lineage
                == crate::sharing_client::SourcePeerLineage::from_capsule(incarnation, session, 7)
                    .expect("lineage")
        );
        // Foreign server or import AAD never opens.
        assert!(open_upstream_capsule(&key, Uuid::new_v4(), &intent, &binding).is_err());
        let mut foreign = intent.clone();
        foreign.scope.import_id = Uuid::new_v4();
        assert!(open_upstream_capsule(&key, server, &foreign, &binding).is_err());
        assert!(open_upstream_capsule(
            &plurx_core::secrets::CredentialKey::generate(),
            server,
            &intent,
            &binding
        )
        .is_err());
        // Capsule identities must equal the durable binding columns exactly.
        let mut swapped = binding.clone();
        swapped.source_session_id = Uuid::new_v4();
        assert!(open_upstream_capsule(&key, server, &intent, &swapped).is_err());
        let mut swapped = binding.clone();
        swapped.source_incarnation_id = Uuid::new_v4();
        assert!(open_upstream_capsule(&key, server, &intent, &swapped).is_err());
        let mut other_request = intent.clone();
        other_request.recipe.source_request_id = Uuid::new_v4();
        assert!(open_upstream_capsule(&key, server, &other_request, &binding).is_err());
        // Lineage carries the same checks as a Start response.
        for epoch in [0, u64::MAX] {
            let bad = upstream(&key, server, &intent, session, incarnation, epoch);
            assert!(
                open_upstream_capsule(&key, server, &intent, &bad).is_err(),
                "{epoch}"
            );
        }
        let bad = upstream(&key, server, &intent, Uuid::nil(), incarnation, 7);
        assert!(open_upstream_capsule(&key, server, &intent, &bad).is_err());
        // The dispatch capsule and the upstream capsule never open as each other.
        let credential = plurx_core::sharing::new_secret().expect("credential");
        let dispatch = seal_dispatch_capsule(
            &key,
            server,
            &intent,
            &credential,
            &"d".repeat(64),
            &endpoint(),
        )
        .expect("dispatch");
        let (opened, viewer, at) =
            open_dispatch_capsule(&key, server, &intent, &dispatch).expect("dispatch opens");
        assert_eq!(opened.expose(), credential.expose());
        assert_eq!((viewer.as_str(), at), ("d".repeat(64).as_str(), endpoint()));
        assert!(open_dispatch_capsule(&key, server, &foreign, &dispatch).is_err());
        assert!(open_dispatch_capsule(&key, server, &other_request, &dispatch).is_err());
        assert!(
            open_dispatch_capsule(&key, server, &intent, &binding.capability_envelope).is_err()
        );
        let mut disguised = binding.clone();
        disguised.capability_envelope = dispatch;
        assert!(open_upstream_capsule(&key, server, &intent, &disguised).is_err());
    }

    struct Fixture {
        state: AppState,
        db: std::path::PathBuf,
        scope: ReceiverCatalogueScope,
        user: i64,
        hash: String,
    }
    struct Route {
        intent: ReceiverSessionIntent,
        owner: ReceiverSourceOwner,
    }

    fn sql(db: &std::path::Path, statements: &[(&str, Vec<rusqlite::types::Value>)]) {
        let connection = rusqlite::Connection::open(db).expect("fixture connection");
        connection
            .busy_timeout(Duration::from_secs(10))
            .expect("busy timeout");
        for (statement, values) in statements {
            connection
                .execute(statement, rusqlite::params_from_iter(values.iter()))
                .expect("fixture statement");
        }
    }
    fn read(db: &std::path::Path, statement: &str, inc: &str) -> String {
        let connection = rusqlite::Connection::open(db).expect("fixture connection");
        connection
            .query_row(statement, [inc], |row| row.get(0))
            .expect("fixture read")
    }

    async fn fixture() -> Fixture {
        let base = crate::test_temp_path(format!("plurx-orphan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).expect("fixture dir");
        let db = base.join("b.sqlite");
        let store = SqliteStore::open(&db).expect("store");
        let state = AppState::new(
            "test".into(),
            Arc::new(store),
            crate::state::Dirs {
                artwork: base.join("artwork"),
                transcode: base.join("transcode"),
                cache: base.join("cache"),
                subs: base.join("subs"),
                runtime_cache: base.join("runtime"),
                renditions: base.join("renditions"),
            },
            "B-live".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        let user = state
            .store
            .create_user("orphan-viewer", "fixture-hash", false)
            .await
            .expect("B user");
        let hash = "c".repeat(64);
        state
            .store
            .create_token(&hash, user.id, None)
            .await
            .expect("B login");
        let scope = scope();
        sql(&db, &[
            ("INSERT INTO settings(key,value) VALUES('sharing_enabled','true')", vec![]),
            ("INSERT INTO sharing_viewers(user_id,viewer_id) VALUES(?1,?2)", vec![user.id.into(), Uuid::new_v4().to_string().into()]),
            ("INSERT INTO sharing_imports(id,source_server_id,catalogue_epoch,source_name,claim_id,remote_grant_id,credential_envelope,endpoints_json,assignment_generation,lifecycle_generation,endpoint_generation,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'Source',?4,?5,'fixture never opened','[]',1,1,1,'active',1000,1000)", vec![scope.import_id.to_string().into(), scope.source_server_id.to_string().into(), scope.catalogue_epoch.to_string().into(), scope.claim_id.to_string().into(), scope.remote_grant_id.to_string().into()]),
            ("INSERT INTO sharing_assignments VALUES(?1,'0',?2,1)", vec![scope.import_id.to_string().into(), user.id.into()]),
        ]);
        Fixture {
            state,
            db,
            scope,
            user: user.id,
            hash,
        }
    }

    /// A blocked B route owned by a dead node, optionally attached.
    async fn route(f: &Fixture, label: &str, attached: bool) -> Route {
        let intent = intent(label, f.user, &f.hash, &f.scope);
        let request = format!("request-{label}");
        let inc = intent.recipe.source_request_id.to_string();
        let principal = PlaybackPrincipal::LocalUser { user_id: f.user };
        let fingerprint = intent.recipe.request_fingerprint().expect("fingerprint");
        let now = clock_ms();
        assert!(matches!(
            f.state
                .store
                .claim_media_session_request(
                    &principal,
                    &request,
                    &fingerprint,
                    &format!("player-{label}"),
                    &inc,
                    now,
                    now + 30_000
                )
                .await
                .expect("claim"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(f
            .state
            .store
            .assign_media_session_request_owner(&principal, &request, &inc, "B-dead", now)
            .await
            .expect("assign"));
        let authority = f
            .state
            .store
            .prepare_receiver_session_authority(intent.clone())
            .await
            .expect("authority")
            .expect("authorized");
        let activation = MediaSessionActivation {
            incarnation_id: inc.clone(),
            session_id: Uuid::new_v4().to_string(),
            principal,
            playback_id: format!("player-{label}"),
            recovery_epoch: Uuid::new_v4().to_string(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some(request.clone()),
            request_fingerprint: fingerprint,
            owner_node_id: "B-dead".into(),
            recipe_json: serde_json::to_string(&intent.recipe).expect("recipe"),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 30_000,
            expected_desired_revision: None,
        };
        let outcome = f
            .state
            .store
            .activate_receiver_media_session(&authority, &activation)
            .await
            .expect("activation")
            .expect("blocked route");
        let owner = ReceiverSourceOwner {
            incarnation_id: intent.recipe.source_request_id,
            session_id: Uuid::parse_str(&activation.session_id).expect("session"),
            owner_node_id: "B-dead".into(),
            owner_epoch: outcome.route.owner_epoch,
            request_id: request,
            lease_expires_at_ms: activation.lease_expires_at_ms,
            now_ms: clock_ms(),
        };
        if attached {
            let local = f
                .state
                .store
                .sharing_identity(clock_ms())
                .await
                .expect("identity");
            let binding = upstream(
                &f.state.sharing.key,
                local.server_id,
                &intent,
                Uuid::new_v4(),
                Uuid::new_v4(),
                3,
            );
            let authority = f
                .state
                .store
                .prepare_receiver_session_authority(intent.clone())
                .await
                .expect("authority")
                .expect("authorized");
            assert_eq!(
                f.state
                    .store
                    .attach_receiver_source(
                        &authority,
                        &ReceiverSourceAttachment {
                            owner: owner.clone(),
                            binding
                        }
                    )
                    .await
                    .expect("attach"),
                ReceiverSourceWrite::Applied
            );
        }
        Route { intent, owner }
    }

    fn age(f: &Fixture, route: &Route) {
        let lease = clock_ms() - RECEIVER_ORPHAN_GRACE_MS - 1_000;
        let inc = route.owner.incarnation_id.to_string();
        sql(&f.db, &[
            ("UPDATE media_sessions SET lease_expires_at_ms=?1 WHERE incarnation_id=?2", vec![lease.into(), inc.clone().into()]),
            ("UPDATE job_leases SET expires_at_ms=?1 WHERE resource='session:'||?2", vec![lease.into(), inc.clone().into()]),
            ("UPDATE media_session_requests SET claim_expires_at_ms=?1 WHERE incarnation_id=?2", vec![lease.into(), inc.into()]),
        ]);
    }

    const ROUTE: &str = "SELECT json_array(s.state,s.owner_node_id,s.owner_epoch,s.publication_ready_at_ms,s.response_json,(SELECT count(*) FROM sharing_relay_upstream b WHERE b.incarnation_id=s.incarnation_id),(SELECT count(*) FROM job_leases j WHERE j.resource='session:'||s.incarnation_id),(SELECT count(*) FROM sharing_delivery_grants g WHERE g.incarnation_id=s.incarnation_id AND g.state='active')) FROM media_sessions s WHERE s.incarnation_id=?1";

    async fn orphan(f: &Fixture) -> ReceiverOrphan {
        let mut page = f
            .state
            .store
            .orphaned_receiver_sessions(None, RECOVERY_BATCH)
            .await
            .expect("inventory");
        assert!(page.unreadable.is_empty());
        page.orphans.pop().expect("one orphan")
    }

    #[tokio::test]
    async fn receiver_orphan_sweeper_skips_live_local_actor() {
        let f = fixture().await;
        let route = route(&f, "live", false).await;
        age(&f, &route);
        // A live local actor for this exact Source request is its owner.
        let wrapper =
            crate::sharing::receiver_source_wrapper(&route.intent.recipe).expect("wrapper");
        let _live = f
            .state
            .sharing
            .receiver_starts
            .register(
                route.intent.clone(),
                route.owner.request_id.clone(),
                "player-live",
                &wrapper,
            )
            .expect("live actor");
        let inc = route.owner.incarnation_id.to_string();
        let before = read(&f.db, ROUTE, &inc);
        let shutdown = tokio_util::sync::CancellationToken::new();
        assert_eq!(
            recover_orphan(&f.state, orphan(&f).await, &shutdown).await,
            OrphanOutcome::Deferred("live_local")
        );
        assert_eq!(
            read(&f.db, ROUTE, &inc),
            before,
            "nothing claimed or retired"
        );
        assert!(f
            .state
            .sharing
            .status()
            .receiver_recovery
            .in_flight
            .is_empty());
    }

    #[tokio::test]
    async fn receiver_orphan_sweeper_never_retires_without_end_receipt() {
        let f = fixture().await;
        let route = route(&f, "attached", true).await;
        age(&f, &route);
        let inc = route.owner.incarnation_id.to_string();
        let state = Arc::new(f.state.clone());
        let shutdown = tokio_util::sync::CancellationToken::new();
        let orphan = orphan(&f).await;
        let attempt = {
            let (state, shutdown) = (state.clone(), shutdown.clone());
            tokio::spawn(async move { recover_orphan(&state, orphan, &shutdown).await })
        };
        // The claim lands; the unreachable Source never answers End.
        tokio::time::timeout(Duration::from_secs(20), async {
            while !read(&f.db, ROUTE, &inc).contains("\"B-live\"") {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("claimed by this node");
        assert_eq!(
            state.sharing.status().receiver_recovery.in_flight,
            vec![route.owner.incarnation_id]
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        shutdown.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(20), attempt)
            .await
            .expect("drain ends the attempt")
            .expect("joined");
        assert_eq!(outcome, OrphanOutcome::Deferred("shutdown"));
        // Claimed at the next epoch, never retired: binding and lease remain.
        let observed: serde_json::Value =
            serde_json::from_str(&read(&f.db, ROUTE, &inc)).expect("route");
        assert_eq!(observed[0], "active");
        assert_eq!(observed[1], "B-live");
        assert_eq!(observed[2], route.owner.owner_epoch + 1);
        assert_eq!(observed[5], 1, "binding kept without a Source End receipt");
        assert_eq!(observed[6], 1, "lease kept");
        assert!(state
            .sharing
            .status()
            .receiver_recovery
            .in_flight
            .is_empty());
    }

    #[tokio::test]
    async fn receiver_orphan_sweeper_never_adopts_producer() {
        let f = fixture().await;
        let pending = route(&f, "pending", false).await;
        let unknown = route(&f, "unknown", false).await;
        age(&f, &pending);
        age(&f, &unknown);
        sql(
            &f.db,
            &[(
                "UPDATE sharing_relay_upstream SET dispatch_envelope=NULL WHERE incarnation_id=?1",
                vec![unknown.owner.incarnation_id.to_string().into()],
            )],
        );
        let before = read(&f.db, ROUTE, &pending.owner.incarnation_id.to_string());
        assert!(before.contains(&MEDIA_SESSION_PUBLICATION_BLOCKED.to_string()));
        let shutdown = tokio_util::sync::CancellationToken::new();
        assert_eq!(sweep_page(&f.state, None, &shutdown).await, Ok(None));
        // The never-dispatched route is retired on the claim's no-send proof:
        // no Start, no registry actor, no publication, delivery or renewal.
        assert!(f
            .state
            .sharing
            .receiver_starts
            .entries
            .lock()
            .expect("registry")
            .is_empty());
        let retired: serde_json::Value = serde_json::from_str(&read(
            &f.db,
            ROUTE,
            &pending.owner.incarnation_id.to_string(),
        ))
        .expect("route");
        assert_eq!(retired[0], "ended");
        assert_eq!(
            retired[3], 0,
            "terminal metadata, never a published producer"
        );
        assert!(retired[4]
            .as_str()
            .expect("receipt")
            .contains("receiver_retirement_v1"));
        assert_eq!(
            (
                retired[5].as_i64(),
                retired[6].as_i64(),
                retired[7].as_i64()
            ),
            (Some(0), Some(0), Some(0))
        );
        // Unknown dispatch owes an End nobody can authenticate: kept, reported.
        let kept: serde_json::Value = serde_json::from_str(&read(
            &f.db,
            ROUTE,
            &unknown.owner.incarnation_id.to_string(),
        ))
        .expect("route");
        assert_eq!(
            (kept[1].as_str(), kept[5].as_i64()),
            (Some("B-dead"), Some(1))
        );
        let status = f.state.sharing.status().receiver_recovery;
        assert_eq!(status.retired_total, 1);
        assert_eq!(status.stranded.len(), 1);
        assert_eq!(
            status.stranded[0].incarnation_id,
            unknown.owner.incarnation_id.to_string()
        );
        assert_eq!(status.stranded[0].reason, "dispatch_unknown");
        assert!(crate::telemetry::prometheus()
            .contains("plurx_sharing_receiver_orphan_total{outcome=\"retired\"}"));
    }

    #[tokio::test]
    async fn receiver_orphan_sweeper_observes_drain() {
        let f = fixture().await;
        let route = route(&f, "drain", false).await;
        age(&f, &route);
        let inc = route.owner.incarnation_id.to_string();
        let before = read(&f.db, ROUTE, &inc);
        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();
        assert_eq!(
            recover_orphan(&f.state, orphan(&f).await, &shutdown).await,
            OrphanOutcome::Deferred("shutdown")
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            receiver_recovery_loop(f.state.clone(), shutdown.clone()),
        )
        .await
        .expect("drain ends the owner at once");
        assert_eq!(read(&f.db, ROUTE, &inc), before, "drain claims nothing");
        // Drain arriving during a page ends the owner after that page.
        let running = tokio_util::sync::CancellationToken::new();
        let owner = tokio::spawn(receiver_recovery_loop(f.state.clone(), running.clone()));
        tokio::time::timeout(Duration::from_secs(20), async {
            while f
                .state
                .sharing
                .status()
                .receiver_recovery
                .last_scan_at_ms
                .is_none()
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("first look");
        running.cancel();
        tokio::time::timeout(Duration::from_secs(5), owner)
            .await
            .expect("drain ends the owner")
            .expect("joined");
        assert_eq!(
            f.state.sharing.status().receiver_recovery.retired_total,
            1,
            "the first look retired the never-dispatched route"
        );
    }
}
