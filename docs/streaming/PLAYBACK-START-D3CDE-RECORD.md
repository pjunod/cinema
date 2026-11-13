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

**Deviation.** The contract (§3.5) named a post-insert `request_row` read to fold; the code has none. The fold's read-back replaces the `desired_row` read of the separate record, so the saving is one write, not one read.

**Per-shape delta** (authority/local/writes): the intent shape 21/2/9 becomes 21/2/8; the other four shapes are unchanged. Method table: `claim_media_session_request` (2,0,1), and `record_desired_selection` is gone.

**Tests.** `store_contract/claim_selection.rs` (sqlite and the three-voter hiqlite cluster): `a_claim_records_the_ask_only_when_it_acquires`, `an_overloaded_or_invalid_claim_records_no_ask`, `concurrent_claims_for_one_request_record_one_ask`. `chunk_06.rs`: `a_create_refused_before_its_claim_records_no_ask`, `an_overloaded_create_records_no_ask`; `an_ask_on_create_is_recorded_before_the_create_is_answered` is unchanged (a create that reaches its claim still records).

**Counterfactuals** (scratch edits on the D3c head, reverted):

- Predicate removed from the hiqlite selection (`WHERE 1 = 1 OR EXISTS`): `an_overloaded_or_invalid_claim_records_no_ask` fails (an overloaded claim recorded an ask); the other two pass. A joined claim never reaches the selection statement, so the predicate is what keeps the overloaded claim (whose insert writes nothing) from recording.
- Up-front `record_desired_selection` restored in `create.rs` and the claim passed `None`: `a_create_refused_before_its_claim_records_no_ask` and `an_overloaded_create_records_no_ask` fail (both found a revision-1 ask).
- The same restored code against the new pin: `create_path_store_operations_are_pinned_per_shape` fails, the intent shape reading 21/2/9 against the pinned 21/2/8.

## 2. D3d — the catalog reads the create's StartPlan

**Built.** `media_pool::CatalogSource { Quorum(Option<&PlanningBinding>), Plan(&StartPlan), PeerView(&PlanningBinding) }` replaces the `expected_binding` parameter of `local_quality_catalog`.

- **`Plan`:** the create captures its `StartPlan` in `CreateStartupBudget` (`capture_create_plan`); `quality_catalog_answer` uses it when the create captured a plan for that file, and reads no store.
- **`PeerView`:** a v2 peer request carrying `expected_binding` reads this node's applied view once through `start_plan::peer_catalog_plan` (view, then authority once, via `owner_plan`); a changed binding or missing source is `CatalogCause::StaleSource` (the sender re-plans through D3b's `replan_once_after_stale`), a store error is `SnapshotUnavailable`.
- **`Quorum`:** the v1 handler, the prepared-VOD owner and any create without a captured plan keep the quorum read and compare against `expected`. The size and mtime `StaleSource` check is kept in every arm.
- `developer.rs` gains the requirement row `create_plan_peer_catalogs` (plan reads and stale replies, from `peer_catalog_counts`).

**Tests** (`start_plan_tests.rs`): `the_create_catalog_answers_from_the_create_plan_and_reads_nothing`, `a_peers_catalog_reads_its_view_and_a_stale_sender_replans`, `the_catalog_sources_are_chosen_in_three_places`.

**Counterfactuals:** see §4.

**Not done.** The pin does not see the catalog: its fixture runs the media pool off, so none of the five shapes builds a catalog. A pool-on shape (via `PUT /api/v1/settings {"cluster_media_pool_enabled": true}`) is the way to pin it and is left open. G1 exposes the peer counters only as totals, not per shape. The per-candidate catalog cache is only worth building if the local catalog compute p90 is at least 100 ms; that was not measured here, so no cache was built.

## 3. D3e — not built: the premise does not hold in code

The contract (§3.5, §4.7) lets the owner skip the held probe on a decode-fact cache hit, on the premise that a seven-field tuple validates the source. In code:

- That tuple (`ProbeFileIdentity::same_tuple`) validates the ffprobe executable, not the source. Source identity (`DecodeSourceIdentity`) is a sha256 of fstat metadata with no content-hash path; hashing multi-GB sources is not viable.
- The cache holds only `DecodeFacts`, never the probe document, and `probe_source_document` deliberately cannot be satisfied by a cache hit (`FreshSourceProbe`).
- The held document is also used for the stored-probe comparison (the `vod_source_rescan_required` refusal), `held_source_clock`, and the Dolby Vision stream and time-base lookup at create time, so skipping it needs the document, not just the facts.

Following the brief (record the gap, keep the probe), the owner keeps its held probe. **Decision for Paul:** a verified-identity memo caching the document, clock and facts keyed by source identity, ffprobe build digest and stored-probe digest would make it possible, but it contradicts the "no full document is cached" design; not built without approval.

## 4. Counterfactual runs, D3d

Each run on the D3d head with one scratch edit, then reverted.

- **Create catalog on the quorum** (`quality_catalog_answer` choosing `Quorum(expected_binding)` where the create has a plan): `the_create_catalog_answers_from_the_create_plan_and_reads_nothing` fails (`[StaleSource]`: the quorum read does not match the plan's binding); `a_peers_catalog_reads_its_view_and_a_stale_sender_replans` passes.
- **Peer handler without its view** (`internal_media.rs` answering `Quorum(None)` for a v2 request): the two behavioural tests above still pass, because both call `local_quality_catalog` with `PeerView` directly; only the source-text pin `the_catalog_sources_are_chosen_in_three_places` fails. **Gap:** nothing drives the handler end to end for the peer arm; a handler-level test needs a signed internal request fixture and is left open.
