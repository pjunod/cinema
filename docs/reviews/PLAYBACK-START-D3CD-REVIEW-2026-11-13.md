# D3c / D3d review — the claim records the ask; the catalog reads the plan

**Status:** done — every finding is dispositioned in the table at the end; the fixes are on `claude/ps-d3cde`.
**Reviewer:** Fable (claude-fable-5-2), adversarial, read-only.
**Date:** 2026-11-13.
**Scope:** `git diff origin/claude/ps-d3b origin/claude/ps-d3cde` (`6dd852ab9..6cdf3c417`), 5 commits (`ccebc0392`, `a6cb43729`, `ad848f0d8`, `836934071`, `6cdf3c417`), 22 files, +1606/−195. Contract: `docs/streaming/PLAYBACK-START-D3-BUILD.md` rev 2 §4.5, §4.6, §5, §8, §12; record `PLAYBACK-START-D3CDE-RECORD.md`; D3b review B1 (`replan_once_after_stale`).
**Method:** source reading over `git show`/`git diff`/`git grep` blobs at `origin/claude/ps-d3cde`. Nothing was built or run; every "would" below is by reading. Line numbers are at the head.
**Verdict:** **changes requested.** D3c's store half is sound on both backends. D3d removes the read that D3b's B1 fix depends on and does not replace its effect: a stale ingress now gets a complete local catalog from its own stale plan, so a peer's `StaleSource` no longer fails the attempt and the re-plan never runs (B1). The test that claims to cover that path hands the failure in by hand (S3).

## Blockers

### B1 — a peer's `StaleSource` no longer re-plans the create; the stale plan is built
`media_pool.rs:1576-1581` answers the local catalog from the captured plan. `note_create_catalog` (`media_pool.rs:729-739`) still sets `refuted` when a peer answers `StaleSource`, but `replan_once_after_stale` (`start_plan.rs:121-127`) acts only on `Err(_)` from the attempt, and the attempt errs only when no candidate was picked (`create.rs:2349-2361`, inside `picked.ok_or_else`). `preparation_catalog_workers` (`create.rs:1857`) takes candidates, not completeness.

On `ps-d3b` a stale ingress got `StaleSource` from its own quorum read, so it had no local candidates, nothing was picked, the create answered 503 and the re-plan ran. Now:

1. A planning key is saved, or the file is rescanned, from another node. This ingress holds a valid proof but has not applied it.
2. The create plans at the old binding *b* from the view. The local catalog is `Plan`: complete, with candidates at *b*.
3. A peer that is ahead reads its view, then Authority once, and answers `StaleSource` (`start_plan.rs:197-214`). The merged result is `complete = false`, `causes = [StaleSource]`, with the local candidates still in it.
4. A local candidate is picked. No `Err`, so no re-plan, the view is not closed, and `refuted` is never consumed.
5. The create claims and builds locally at *b*: `validate_candidate_planning_binding` compares the request with the plan's own binding and passes.

This is §5's "ingress replica behind" row and §4.3's "a peer `StaleSource` … re-plans once from Authority". The quorum fact that the plan is dead is in hand and dropped, and every later create on this node repeats it until the replica applies. It is the D3b B1 hole reopened in the other direction: before, every create failed; now every create builds the superseded revision.

**Fix:** make the refutation itself end the attempt. After the selection catalog returns (`create.rs:2174-2177`), if the create holds no claim, its plan came from the view and the catalog noted `StaleSource`, return the typed `quality_catalog_unavailable` before any pick; `replan_once_after_stale` then does the rest unchanged. A plan read from Authority that a peer calls stale is a real change between two reads and may proceed or fail as today. Add the §8 "stale ingress" test at this level (S3).

## Should fix

### S1 — stale-ingress detection now needs a peer that is ahead
`media_pool.rs:1576-1581`, `start_plan.rs:158-175`. `owner_plan(.., Some(expected))` returns the view plan with no authority read when the peer's view also holds *b*. After D3d nothing reads Authority before the claim when: no v2 media peer is reachable inside the catalog deadline; every answering peer's view is as far behind as the ingress's; or the only peer that is ahead times out (`PeerDeadline`, not `StaleSource`). With a local owner the session is then built at the view's revision. On `ps-d3b` the ingress's own quorum read caught all three. Contract §4.6 asks for this shape, so this is a decision rather than a defect, but §5 has no row for it and the record does not say it.
**Fix:** either record it as accepted (the plan is bounded by the proof and both floors, as a non-negotiated create's already is), or, when the plan is from the view and no peer answered complete at *b*, confirm with one authority read as `confirm_unbound_remote` does. The second costs the read D3d removed on nodes with no peers. For Paul.

### S2 — asks are now ordered by claim, not by arrival
`create.rs:3348-3357` (the ask is only captured) and `:3645-3672` (written at the claim). The upsert used to be the create's first step; it now follows the plan and the catalog fan-out, up to the catalog deadline and twice that after a re-plan. Two creates for one `playback_id` with different asks, A then B: if B (the later ask) reaches its claim first, B records revision 1 and A records revision 2. The desired row ends on the older ask, A's activation matches revision 2 and takes the pointer, and B's is refused. Before D3c B always won. A client that aborts A cancels it before its claim (`OwnedCreate`), so this needs an un-aborted or proxied first request; I found nothing in `create.rs` that serialises creates per playback.
**Fix:** stamp the ask with the create's arrival time and have the folded upsert refuse to replace a row with a later `updated_at_ms`, answering that create as superseded; or accept it in §12-6 with a test that pins the order.

### S3 — the "whole path" test injects the failure it should observe; the peer arm is untested end to end
`start_plan_tests.rs`, `a_peers_catalog_reads_its_view_and_a_stale_sender_replans`: the first attempt calls `note_create_catalog(&wire)` and then returns a hand-built `Err(quality_catalog_unavailable)`. The real create returns no such error once its local catalog is complete (B1), so the test passes on code where the path is dead. Record §4 already shows the other half: with the v2 handler answering `Quorum(None)` both behavioural tests pass and only the source-text pin fails. The handler gap is not a blocker on its own; with B1 it is why B1 was not seen.
**Fix, minimal:** one test in create scope with a view plan, a pool whose peer transport returns a serialized `QualityCatalogResult { complete: false, causes: [StaleSource] }`, and a local catalog that is complete from the plan. Assert that the attempt errs before its claim, that `stale_replans` moves by one, and that the second attempt's plan is `ViewSource::Authority`. It fails on this head. For the handler, drive `quality_candidates_v2` with `expected_binding` set after a planning key is saved and assert `StaleSource` and one `binding_mismatch` read; the unsigned-request fixture at `internal_media.rs:778` shows the harness, and a signed fixture is the only missing piece.

## Notes

- **N1 — the captured plan outlives its attempt.** `create_plan_for` (`media_pool.rs:712-724`) matches on scope and file id only, and `spawn_create_worker` (`:511`) clones the budget into the start worker. Any later catalog call in that scope for the same file answers from the create's plan with no check, where it used to fail safe on a quorum compare. Today's callers are the two in the create (`create.rs:2176`, `:4109`); I did not find `preparation.rs:1910` inside a create scope but did not prove it. Clear the plan when the create settles, or pass it as an argument.
- **N2 — the post-claim catalog changed too.** `create.rs:4109` (`optional_metadata`) is now `Plan`; it used to compare by quorum and come back incomplete after a mid-create save. Consistent with one revision per session, but not in the record.
- **N3 — the rollback claim is unverified.** `hiqlite_sessions.rs:2206-2212` says statements in one `txn` do not roll back together; `hiqlite.rs:1750-1751` speaks of an explicit rollback on a failed first statement. Either way the code is safe: the predicate holds per statement, and a claim committed without its ask returns `Err`, which the armed guard settles (`session_guard.rs:251-252`). State which is true in the record.
- **N4 — the read-back is a separate read.** `hiqlite_sessions.rs:3275-3284`: a concurrent ask for the same playback between the transaction and `desired_row` is answered as this create's revision. `record_desired_selection` had the same shape, so it is not a regression; `RETURNING revision` would close it and save the read.
- **N5 — the inventory entry is unexecuted.** `store_contract.rs:21347` keeps 559 because the test already fails on main, so the name-set assertion behind the count never runs and nothing checks the new `MEDIA_SESSION_METHODS` entry.
- **N6 — paperwork.** `PLAYBACK-START-D3-RECORD.md` changed only its status line; its §10 table still shows the intent shape at 9 writes. No commit carries a `Regression-Test:` line (the coordinator puts them on the merge message); the counterfactuals in record §1 and §4 are stated and plausible by reading.
- **N7 — no p90 and no pool-on pin.** Declared. §12-5 makes the cache conditional on that number, so "no cache" is undecided, not decided.

## D3e — not built

The judgement is correct. `ProbeFileIdentity::same_tuple` (`decode_facts.rs:901-920`) is the probe executable's `fstat` tuple, the cache holds only `DecodeFacts`, and `FreshSourceProbe` says "the document is transient; only facts enter cache" (`:4144-4148`). The contract's premise (§4.7, §12-C6) does not hold, and its fallback clause ("records the gap and keeps the probe") is what was done.

On the proposed memo (document, clock and facts, keyed by source identity, ffprobe digest and stored-probe digest), for Paul:

- It caches the full document, which the design rules out by name. That rule exists so a hit can never stand in for a look at the source.
- Source identity is `fstat` metadata only, so a hit is exactly as strong as size, mtime, ctime, device and inode. That is the same evidence the contract already accepted for the tuple, but it should be said plainly: no content is checked on a hit.
- A narrower form keeps the rule: cache the three things the create derives from the document (the stored-probe comparison verdict, the Dolby Vision stream index and time base, and the clock's inputs) as typed facts beside `DecodeFacts`, under the same key, with an `fstat` re-check on every hit and the existing revalidation interval as the bound.
- Measure first. D3a's numbers should say what the held probe costs per start; if it is not a large share of the remaining create time, neither form is worth the new invariant.

## Checked, no finding

- **D3c predicate, both stores.** `request_id`, this `incarnation_id`, `state = 'starting'`, `claim_expires_at_ms > now`, under the owner predicate, in the insert and the re-acquire paths (`hiqlite_sessions.rs:2236-2241`; `sqlite/sessions.rs:304-307`). A conflicting insert, an overloaded insert, a joined or resolved row and a fingerprint conflict never match it. SQLite runs it inside the claim transaction and requires one changed row, for the inserted and the re-acquired outcome.
- **Retries and incarnations.** A retry with the same request id joins or replays and records nothing; a re-acquired failed or expired claim records under its new incarnation, and a repeated digest keeps its revision. The create re-claims with its own incarnation only.
- **Failure after the claim.** A store error after the commit drops the armed guard, which fails that exact incarnation; no `starting` row is stranded.
- **Readers of the desired row.** `control.rs:3043` still writes its own; `preparation.rs:2898` and `playback_control.rs:6178` read for a live session, which only an acquired create produces. Activation compares the revision the claim returned (`create.rs:4858`). A create refused before its claim was never reported accepted and each such answer is retried by the clients; I found no client-facing read of the ask after a refusal.
- **Peer arm.** View, then Authority once, then `StaleSource`; a gone source is `StaleSource`; a store error is `SnapshotUnavailable`; the read runs inside the catalog's `timeout_at` (`media_pool.rs:2185`).
- **Mixed versions.** An old peer on v2 still reads by quorum and compares; an old ingress with no binding gets `Quorum(None)`; the v1 route is unchanged; the 404 fallback to v1 is unchanged.
- **Prepared-VOD owner** keeps `Quorum(Some(&catalog.binding))` (`media_pool.rs:2262-2268`). **Processed descriptions** are computed in every arm.
- **Formal rebind.** One anchor occurrence before and after; the hunks in `store_contract.rs` are the module declaration above it and one method name, as the commit says. Honest.
- **Docs budget.** `6cdf3c417` only shortens index and plan-row text; the record is linked from the D3 contract row and nothing is deleted. Honest.

## Dispositions (2026-11-13)

Coordinator's decisions; built by claude-sonnet-5-5 (session 01BCWL9xwmpSjitHqZMTRAht) on `claude/ps-d3cde`. Counterfactuals are one scratch edit each on the fix head, reverted.

| Finding | Disposition | What changed | Proof |
|---|---|---|---|
| B1 | Fixed | `media_pool::create_plan_refuted()` (view plan, unclaimed, a catalog noted `StaleSource`, not re-planned) is checked right after the selection catalog in `create.rs`; it ends the attempt with `quality_catalog_unavailable` before any pick, and `replan_once_after_stale` re-plans once from Authority and closes the view. | `a_peers_stale_answer_ends_a_view_plan_before_any_candidate_is_picked` drives the real `create` with a view plan, a complete local catalog and a queued peer `StaleSource`: one re-plan, both fan-outs run, view closed. Without the check it fails (`re-planned the create once`, left 0, right 1). |
| S1 | Accepted, recorded | Decision in the D3CDE record §2: stale-ingress detection relies on an ahead peer; otherwise the view's proof and both floors are the guard (contract §4.6). The residual window is stated: a create made while the replica is behind, no peer ahead, and the planning keys changed in that interval; the next create after the replica applies re-plans. An authority read for the no-peer case was declined (it costs the read D3d removed). | Record only. |
| S2 | Fixed | The ask carries the request's arrival time (taken once before the re-plan loop); both stores' upsert has `WHERE media_playback_desired.updated_at_ms <= excluded.updated_at_ms` on the conflict arm, so an older arrival replaces nothing; the claim answers with the standing row and a create whose digest lost is refused with `409 selection_superseded`. | `an_earlier_but_slower_create_does_not_replace_a_newer_ask` (both backends: older arrival after a newer one, later and equal stamps replace, a re-acquired claim follows the rule). With the arrival predicate replaced by `WHERE 1 = 1` it fails (the older arrival replaced the newer ask: revision 2, digest `a…`, against the standing revision 1, `b…`); the other three claim tests pass. |
| S3 | Fixed | The hand-built `Err` test is removed; the peer arm keeps its direct checks as `a_peers_catalog_reads_its_view_and_answers_a_stale_binding`; the end-to-end test above covers the create path through a test-only peer-answer seam on `MediaPool`. | As B1. The handler-level gap (a signed request to `quality_candidates_v2`) is still open: `the_catalog_sources_are_chosen_in_three_places` pins its source choice by text. |
| N1 | Fixed | `capture_create_plan` returns a `CapturedPlan` held by the attempt; dropping it clears the budget's copy. `spawn_create_worker` clones the budget without the plan. | `a_captured_plan_does_not_outlive_its_attempt`: complete inside the attempt, `StaleSource` (by quorum) in a worker and after the drop. With the clear removed it fails on the worker and post-drop answers. |
| N2 | Recorded | The post-claim `optional_metadata` catalog is inside the attempt and answers from the plan: correct, one revision per session. | Record §2. |
| N3 | Corrected | hiqlite 0.14's writer rolls the whole transaction back when a statement errors; a statement matching no row is a success and leaves its sibling committed. The comment said the opposite and is corrected; the predicate is still needed. | Checked against `store/state_machine/sqlite/writer.rs`; record §1. |
| N4 | Recorded as pre-existing | `RETURNING` is not available inside the hiqlite `txn` here and the superseded case needs the standing row when the upsert returns nothing, so the read-back stays; `record_desired_selection` had the same shape; the digest check refuses a create that read another's row. | Record §1. |
| N5 | Recorded only | Inventory repair is out of scope. | Record §1. |
| N6 | Fixed | D3 record §10: intent column writes 8 (`record_desired_selection` gone, claim 2/0/1, total 21/2/8). Regression-Test lines go on the merge message. | Doc. |
| N7 | Recorded | "No cache" is undecided pending a pool-on p90 measurement. | Record §2. |
| D3e | Not built | The reviewer's narrower alternative (typed derived facts with an `fstat` re-check on a hit) and "measure the held probe's cost first" are recorded for Paul. | Record §3. |

