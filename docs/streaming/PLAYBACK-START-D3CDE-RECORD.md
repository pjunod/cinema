# Playback start — D3c, D3d, D3e build record

**Status:** open — what lanes D3c and D3d of [PLAYBACK-START-D3-BUILD.md](PLAYBACK-START-D3-BUILD.md) built on `claude/ps-d3cde` (from `claude/ps-d3b`), the D3e lane that was not built, and where each departs from that contract. The per-shape table is in [PLAYBACK-START-D3-RECORD.md](PLAYBACK-START-D3-RECORD.md) §10; section numbers (§3.5, §12-6 …) refer to the contract.
**Author:** Claude (claude-sonnet-5-5, session 01BCWL9xwmpSjitHqZMTRAht), 2026-11-12.

## 1. D3c — the ask is recorded by the claim

**Built.** `MediaSessionStore::claim_media_session_request_recording(.., desired: Option<&DesiredSelectionWrite>) -> (MediaSessionRequestClaim, Option<DesiredOwnership>)`; the old `claim_media_session_request` is a provided default that passes `None`. The create passes its ask (digest and canonical form of a validated intent) to the claim instead of calling `record_desired_selection` first.

- **hiqlite:** the claim insert (or the re-acquire UPDATE) and the selection upsert run in one `SessionClient::txn`. The upsert is `INSERT .. SELECT .. WHERE EXISTS (the claim row: this principal, request id and incarnation, state = starting, unexpired)`, so it writes only when this attempt owns a live claim; `txn` does not roll statements back together, which is why the selection carries its own predicate. An `Acquired` claim then reads the row back, erroring if it vanished.
- **SQLite twin:** the same upsert inside the claim transaction, with the same predicate, requiring one changed row, for the inserted and the re-acquired outcome.
- **Revision semantics unchanged:** an unchanged digest keeps the revision; a changed digest advances it.
- **Only an acquired claim records.** A joined, conflicting, overloaded or invalid claim records nothing and returns `None`.

**Behaviour change (contract §12-6).** A create refused before its claim (invalid intent or capabilities, the HDR burn guard, the HEVC copy-transport refusal, `quality_catalog_unavailable`, an overloaded or joined claim) records no ask; before, it recorded one on the way to its refusal. A create that reaches its claim and fails later still records it. `after_create_ask_recorded` now fires after the claim is acquired.

**Arrival order (review S2).** The claim follows the plan and the catalog fan-out, so two creates for one playback can reach their claims in the opposite order to their arrival. The ask carries the request's arrival time (`DesiredSelectionWrite::arrived_at_ms`, taken once before the re-plan loop, so a re-planned attempt keeps its place) and the upsert's conflict arm has `WHERE media_playback_desired.updated_at_ms <= excluded.updated_at_ms`: an older arrival replaces nothing, an equal or later one replaces. The claim answers with the row that stands; a create whose digest is not the standing one is refused after its claim with `409 selection_superseded` (the armed guard settles the claim as failed). Stamps are the ingress node's wall clock; between nodes the order is only as good as their clocks, which the desired row's `updated_at_ms` already assumed.

**Deviation.** The contract (§3.5) named a post-insert `request_row` read to fold; the code has none. The fold's read-back replaces the `desired_row` read of the separate record, so the saving is one write, not one read.

**Per-shape delta** (authority/local/writes): the intent shape 21/2/9 becomes 21/2/8; the other four shapes are unchanged. Method table: `claim_media_session_request` (2,0,1), and `record_desired_selection` is gone.

**Transaction semantics (review N3).** Checked against hiqlite 0.14 (`store/state_machine/sqlite/writer.rs`): a statement that *errors* rolls the whole transaction back and `txn` returns `Err`; a statement that matches no row is a success and leaves its sibling committed. The earlier comment said statements "do not roll back together"; that was wrong and is corrected. The predicate is still what keeps an ask from landing without its claim, because a zero-row claim insert (overloaded, joined) is not an error. A claim committed without its ask would return `Err` from the read-back, which the armed guard settles.

**Read-back (review N4).** The ask is read back after the claim, a separate read. `RETURNING` is not available to a statement inside a hiqlite `txn` here, and the superseded case needs the standing row when the upsert returns nothing, so the read stays. `record_desired_selection` had the same shape, so it is not a regression; a concurrent ask for the same playback between the transaction and the read is answered with that row, and the create's digest check above refuses it as superseded.

**Inventory (review N5, recorded only).** `contract_inventory_matches_every_store_method` already fails on main (585 declared against a pinned 559, listed in `UNIT-COVERAGE-REPAIR.md`), so the count stays at main's and the name-set assertion behind it never runs; nothing checks the new `MEDIA_SESSION_METHODS` entry. The repair is out of scope.

**Tests.** `store_contract/claim_selection.rs` (sqlite and the three-voter hiqlite cluster): `a_claim_records_the_ask_only_when_it_acquires`, `an_overloaded_or_invalid_claim_records_no_ask`, `concurrent_claims_for_one_request_record_one_ask`, `an_earlier_but_slower_create_does_not_replace_a_newer_ask`. `chunk_06.rs`: `a_create_refused_before_its_claim_records_no_ask`, `an_overloaded_create_records_no_ask`; `an_ask_on_create_is_recorded_before_the_create_is_answered` is unchanged (a create that reaches its claim still records).

**Counterfactuals** (scratch edits on the D3c head, reverted):

- Predicate removed from the hiqlite selection (`WHERE 1 = 1 OR EXISTS`): `an_overloaded_or_invalid_claim_records_no_ask` fails (an overloaded claim recorded an ask); the other two pass. A joined claim never reaches the selection statement, so the predicate is what keeps the overloaded claim (whose insert writes nothing) from recording.
- Up-front `record_desired_selection` restored in `create.rs` and the claim passed `None`: `a_create_refused_before_its_claim_records_no_ask` and `an_overloaded_create_records_no_ask` fail (both found a revision-1 ask).
- The arrival predicate removed (hiqlite and sqlite): `an_earlier_but_slower_create_does_not_replace_a_newer_ask` fails (the older arrival replaced the newer ask: revision 2, digest `a…`, where revision 1, `b…`, stands); the other three claim tests pass.
- The same restored code against the new pin: `create_path_store_operations_are_pinned_per_shape` fails, the intent shape reading 21/2/9 against the pinned 21/2/8.

**Integration follow-up (Codex, 2026-11-13).** Fast-lane run 7392 exposed
`selection_superseded` missing from the surface contract's
`create_codes_outside_the_table` list. It is now listed there: this 409 ends
the superseded ask and must not retry it. Web, Apple and Android fence an
obsolete local request before surfacing its failure; a still-current request
can report that another actor's newer selection won. No retry-list expansion
is needed or made.

## 2. D3d — the catalog reads the create's StartPlan

**Built.** `media_pool::CatalogSource { Quorum(Option<&PlanningBinding>), Plan(&StartPlan), PeerView(&PlanningBinding) }` replaces the `expected_binding` parameter of `local_quality_catalog`.

- **`Plan`:** the create captures its `StartPlan` in `CreateStartupBudget` (`capture_create_plan`); `quality_catalog_answer` uses it when the create captured a plan for that file, and reads no store.
- **`PeerView`:** a v2 peer request carrying `expected_binding` reads this node's applied view once through `start_plan::peer_catalog_plan` (view, then authority once, via `owner_plan`); a changed binding or missing source is `CatalogCause::StaleSource` (the sender re-plans through D3b's `replan_once_after_stale`), a store error is `SnapshotUnavailable`.
- **`Quorum`:** the v1 handler, the prepared-VOD owner and any create without a captured plan keep the quorum read and compare against `expected`. The size and mtime `StaleSource` check is kept in every arm.
- `developer.rs` gains the requirement row `create_plan_peer_catalogs` (plan reads and stale replies, from `peer_catalog_counts`).

**Stale peers end the attempt (review B1).** The first build let a stale view plan through: the local catalog, built from the plan, stayed complete, so a peer's `StaleSource` no longer failed the attempt and `replan_once_after_stale` never ran. After the selection catalog returns, `media_pool::create_plan_refuted()` (view plan, unclaimed, a catalog noted `StaleSource`, not yet re-planned) ends the attempt with `quality_catalog_unavailable` before any candidate is picked; the create then closes the view and plans once from Authority. A plan read from Authority that a peer calls stale proceeds as before.

**The captured plan is bound to its attempt (review N1).** `capture_create_plan` returns a `CapturedPlan` that the attempt holds; dropping it clears the budget's copy, so a catalog call after the attempt (or in a later one) reads by quorum. `spawn_create_worker` clones the budget without the plan, so the start worker never carries it.

**The post-claim catalog (review N2).** `optional_metadata`'s catalog (`create.rs`, after the claim) is inside the attempt and answers from the plan, where it used to compare by quorum and come back incomplete after a mid-create save. That is correct: one revision per session, the plan's, from the claim to activation; the viewer's next create re-reads.

**Decision (review S1): a stale ingress is caught only by a peer that is ahead.** The old quorum read caught a stale view plan on every create; now nothing reads Authority before the claim unless a media peer's view or Authority holds another binding. A create is not re-planned when no v2 media peer answers inside the catalog deadline, when every answering peer's view is as far behind as the ingress's, or when the only peer that is ahead times out (`PeerDeadline`, not `StaleSource`). Then the session is built at the view's revision. Accepted, as contract §4.6 shapes it: the plan is bounded by the view's proof and its two floors (the same bound a create without a catalog already has), and confirming with one authority read when no peer answered complete at the binding (as `confirm_unbound_remote` does) was declined because it costs the read D3d removed on every node with no peers. The residual window is a create made while the ingress's replica is behind, no peer is ahead, and the planning keys changed in that interval; the next create after the replica applies re-plans correctly.

**Tests** (`start_plan_tests.rs`, `chunk_06.rs`): `the_create_catalog_answers_from_the_create_plan_and_reads_nothing`, `a_peers_catalog_reads_its_view_and_answers_a_stale_binding` (the peer arm, direct), `the_catalog_sources_are_chosen_in_three_places`, and `a_peers_stale_answer_ends_a_view_plan_before_any_candidate_is_picked`, which drives the real `create` with a view plan, a complete local catalog and a queued peer answer of `StaleSource` (a test-only seam on `MediaPool`), and asserts one re-plan, both fan-outs run and the view closed. The earlier whole-path test, which built the `Err` by hand, is removed.

**Counterfactuals:** see §4.

**No cache is undecided (review N7).** A per-candidate catalog cache is only worth building if the local catalog compute p90 is at least 100 ms. That has not been measured, so "no cache" is open pending a pool-on measurement, not a decision.

**Not done.** The pin does not see the catalog: its fixture runs the media pool off, so none of the five shapes builds a catalog. A pool-on shape (via `PUT /api/v1/settings {"cluster_media_pool_enabled": true}`) is the way to pin it and is left open. G1 exposes the peer counters only as totals, not per shape. The per-candidate catalog cache is only worth building if the local catalog compute p90 is at least 100 ms; that was not measured here, so no cache was built.

## 3. D3e — not built: the premise does not hold in code

The contract (§3.5, §4.7) lets the owner skip the held probe on a decode-fact cache hit, on the premise that a seven-field tuple validates the source. In code:

- That tuple (`ProbeFileIdentity::same_tuple`) validates the ffprobe executable, not the source. Source identity (`DecodeSourceIdentity`) is a sha256 of fstat metadata with no content-hash path; hashing multi-GB sources is not viable.
- The cache holds only `DecodeFacts`, never the probe document, and `probe_source_document` deliberately cannot be satisfied by a cache hit (`FreshSourceProbe`).
- The held document is also used for the stored-probe comparison (the `vod_source_rescan_required` refusal), `held_source_clock`, and the Dolby Vision stream and time-base lookup at create time, so skipping it needs the document, not just the facts.

Following the brief (record the gap, keep the probe), the owner keeps its held probe. **Reviewer's narrower alternative.** Cache only what the create derives from the document (the stored-probe comparison verdict, the Dolby Vision stream index and time base, and the clock's inputs) as typed facts beside `DecodeFacts`, under the same key, with an `fstat` re-check on every hit and the existing revalidation interval as the bound. That keeps the "no full document is cached" rule. A hit is still only as strong as size, mtime, ctime, device and inode; no content is checked. Measure first: D3a's numbers should say what the held probe costs per start, and if it is not a large share of the remaining create time neither form is worth the new invariant.

**Decision for Paul:** a verified-identity memo caching the document, clock and facts keyed by source identity, ffprobe build digest and stored-probe digest would make it possible, but it contradicts the "no full document is cached" design; not built without approval.

## 4. Counterfactual runs, D3d

Each run on the D3d head with one scratch edit, then reverted.

- **Create catalog on the quorum** (`quality_catalog_answer` choosing `Quorum(expected_binding)` where the create has a plan): `the_create_catalog_answers_from_the_create_plan_and_reads_nothing` fails (`[StaleSource]`: the quorum read does not match the plan's binding); `a_peers_catalog_reads_its_view_and_a_stale_sender_replans` passes.
- **Peer handler without its view** (`internal_media.rs` answering `Quorum(None)` for a v2 request): the two behavioural tests above still pass, because both call `local_quality_catalog` with `PeerView` directly; only the source-text pin `the_catalog_sources_are_chosen_in_three_places` fails. **Gap:** nothing drives the handler end to end for the peer arm; a handler-level test needs a signed internal request fixture and is left open.
- **B1, refutation check removed** (`if false && create_plan_refuted()`): `a_peers_stale_answer_ends_a_view_plan_before_any_candidate_is_picked` fails at "the peer's refutation re-planned the create once" (0 re-plans, not 1): the create picked the local candidate and went on.
- **N1, plan not cleared on drop:** `a_captured_plan_does_not_outlive_its_attempt` fails (the worker's and the post-attempt catalog answer from the plan instead of `StaleSource` by quorum).
