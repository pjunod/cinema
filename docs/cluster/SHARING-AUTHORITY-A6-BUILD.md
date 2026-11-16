# Sharing authority through the cluster layer — A6 build plan

**Status:** built (plan rev 5): M1, M2 and M3 built and adversarially reviewed (Fable); landing in progress · **Owner:** Claude Opus 5.5, session_01BCWLA21tCtxWeNBWRHWsWn ·
**Implements:** the cluster layer audit (PR #1604) finding D4, action item A6 ·
**Base:** stacked on #1598 (`claude/sharing-listener-unknown-not-disabled`), on top of `main @ 8785492be`

## Problem

Shared-library code proves its own cluster authority, and treats a failed or "not ready" proof as a revocation:
- **Source body monitor** (`http/shared_library.rs` `guard_source_response` → `source_content_current`): every second for each open stream it makes four linearizable reads under a 1 s timeout. Any `Err`, any timeout, and a member floor that isn't ready all become `false`, which cancels the body and settles custody as `Revoke`.
- **Receiver monitor** (`retain_accepted_connection` → `current_delivery_attachment`): any store `Err` becomes `Unresolved`, and the session retires as `ReceiverRetirementReason::Revoked`.
- **Receiver start owner** (`run_owner`, `shared_receiver_playback.rs:2040-2068`, retired at `:586-594`), and the **resource and direct opens** (`:2355-2361`, `shared_receiver_direct.rs:310-316`): any error retires as `Revoked`, including a remote Source round-trip failure.
- **`receiver_content_current`**: uses `current_catalogue_scope`, which is `false` for a saturated local semaphore, an unreachable Source and a 429 (`sharing.rs:2020-2029`).

The cost on a production voter, 2026-11-14: one slow quorum confirmation ends live shared streams as revoked.

## Rules

- **Only an affirmative read of the grant, switch, items or files is a revocation.** The member floor never revokes. Its `NotReady` is what a leaderless window, a stale heartbeat, or a join or removal in flight returns (`membership.rs:3410-3418`, `:3497-3506`, `:3223-3227`).
- **An unreadable or not-ready proof is an outage of this node.** It is resolved through the same `SessionGrace` budget that HLS and Live TV use. Sharing computes no deadline of its own.
- **An outage that outlasts the grace stops bytes**, exactly as a revocation does: the body and transport are cancelled. Only the recorded ending and the custody settle mode differ. The exposure bound equals local sessions': fence detection plus one 5 s budget, where today it is about 2 s.
- **Admission stays fail-closed.** `peer_guard`, `authority()` and the start paths keep their typed, retryable 503s. Only sessions already running get the grace.

## M1 — classified observations and one outage policy (the user-visible fix)

1. **`SharingObservation { Current, Revoked, Unknown, Ending }`** (`sharing.rs`), classified per predicate term, not per call. A table in the PR lists every site.
   - **Switch, grant, items, files:** `Ok(false)` or an affirmative absence is `Revoked`. `Err` or timeout is `Unknown`.
   - **Member floor:** returns `Ready` or `NotReady`. `NotReady` and `Err` are both `Unknown`.
   - **`receiver_delivery` and `receiver_source_binding` absent because of a time or lease term** (`sharing_receiver_sessions.rs:521`, `sharing_receiver_delivery.rs:112`): `Unknown`. Their SQL is split so that "row absent or grant revoked" is distinguishable from "lease or deadline lapsed".
   - **`receiver_content_current`:** unchanged in M1. It is only an admission point: it gates in-memory catalogue and artwork responses before they return, and no session ends on it. Failing closed with a typed, retryable 503 is the admission rule. Moving it to `scope_proof` (`sharing.rs:2044`) helps M2's read budget, not session survival.
   - **`open_sharing` failure** (`:826-836`): a key or rewrap state, so `Unknown`.
   - **Local stop or retirement already in progress** (`:778-784`, `:853`, `:858-866`): `Ending`. The monitor stops without starting a retirement.
2. **The classification travels in `ReceiverStartError`.** It gains `Unknown` and `PeerUnavailable` alongside the `Revoked`-class errors. The owner loop and the resource and direct opens use it.
   - A Source peer failure (`PeerUnavailable`) is not this node's serving outage, and it does not go through `SessionGrace`.
   - The owner retries the status exchange (every 10 s) inside the 30 s lease (`shared_receiver_playback.rs:2011-2018`). It ends the session only when the lease can no longer be renewed, with a "source unavailable" ending and never `Revoked`.
3. **Generalise `SessionGrace`** (`serving_fence.rs`): add `resolve_from(lost_at, recovered: impl Future<Output = Option<u64>>)`.
   - `resolve_loss_from` becomes the watch-backed case of it.
   - A span where authority is current but reads fail passes "the next observation is not `Unknown`" as its recovery. Both kinds of span then spend one shared budget, and flapping is accounted once.
   - The cancel-safety rule stays: one span, one wait.
   - A `Current` answer is discarded if the loss generation moved during the read.
   - A read-failure span recovers only when its next observation is not `Unknown` **and** the fence is ready at the same loss generation.
   - A lapsed receiver lease can't be recovered by re-observation (renewal itself requires `lease > now`, `:2046`). Its `Unknown` spends the grace and ends as an outage; the tests expect that.
   - `UnknownSpan` is the thin per-monitor holder of that span.
   - The listener drops #1598's private `unreadable_since` and uses `UnknownSpan`, so #1598's grace logic becomes the shared one.
4. **Both new endings go through the existing teardown, with an existing durable reason.**
   - Outage expiry and source-unavailable both call `begin_retirement` → `retire_exact`. That joins the start and the bodies, sends Source End, and retires durably as soon as writes succeed. The orphan path stays the fallback only for a stalled retirement, as today.
   - Orphan recovery can't be the route: it skips a live local owner (`shared_receiver_orphans.rs:244-245`, slot released by `mark_retired()` in `retire_owned`, `shared_receiver_retirement.rs:362-381`), and when it does run it takes about two minutes (30 s lease, 60 s `RECEIVER_ORPHAN_GRACE_MS`, up to 30 s tick).
   - No new durable reason: `media_sessions.terminal_reason` has a CHECK (`hiqlite_sessions.rs:225-226`, `:299-301`, `session_principal_rebuild.sql:127`).
   - The durable reason is **`Replaced`**, the value orphan retirements already record for an ending that isn't a revocation (`shared_receiver_orphans.rs:384`). M1's tests check what a viewer sees for it.
   - The in-memory ending mode alone records outage versus source-unavailable, split from the durable reason, after the precedent of `Terminal::AuthorityFenced`.
   - Correction to rev 1: `retirement_cuts_transports()` is a negative match exempting `Deleted`, `Superseded` and `Replaced` (`shared_receiver_playback.rs:201-207`). The new endings must cut the transport explicitly through their in-memory mode, not through the reason.
5. **What M1 doesn't change:** the per-second four-read poll survives until M2 and M3. M1 changes what its answers mean, not how often it asks.
6. **Tests:** the real monitors (source body, receiver connection, owner loop, listener), each driven by a store that times out, errors, returns floor `NotReady`, or returns a lapsed lease, then recovers inside and outside the grace. Assert:
   - no revocation and no durable `Revoked` inside the grace;
   - bytes stop at grace expiry;
   - an affirmative grant revocation ends the stream at once.

   `Regression-Test:` lines for each, and a mutation check of the classification.

## M2 — one cached advisory answer for the member floor

- A single-flight cached `sharing_member_floor_ready` **advisory bool**, per `SharingMemberFloor`, used only by the monitors and `authority()`.
- It is invalidated on a change in the serving-loss generation or the local membership snapshot, and it is never older than `SOURCE_MEMBER_OBSERVATION_MAX_AGE_MS` (5 s). The max age is the real bound, because readiness also depends on replicated heartbeat, capability and join rows that neither invalidator sees (`membership.rs:3157-3190`, `:3727-3736`).
- Only a ready answer is kept for later callers. A not-ready answer serves only the callers already waiting on its read, so a floor blip ends with the next look rather than spending the monitors' outage grace. A failed read likewise answers only the callers already waiting on it and is never kept; a read runs on its own task with its own 1 s bound, so a caller that gives up does not cancel it for the callers behind it; while the serving fence is not ready the read is fresh and nothing is kept.
- Its readers are the body monitors and the read-only admission points: `authority()` and `source_content_current` (catalogue, artwork and body start). A ready answer can admit for up to 5 s after a heartbeat, capability or join change; an error, a timeout or not-ready still refuses with the typed 503.
- `observe_source_admission_members` and `observe_ingress_custody_members` keep their fresh reads, because their `write_guard_for` rejects observations older than 5 s (`:3306`). The "never a cached readiness permission" rule therefore still holds for permissions; the cache is advisory evidence only (`:3192-3194`, `:11621-11623`).

## M3 — change-driven re-observation

Rev 5: rev 4 detailed from the code after M1 and M2 were built, then adversarially reviewed (Fable: sound after fixes; four must-fix and seven should-fix findings, all folded in below). Every sharing read is a linearizable `query_consistent_map` (`hiqlite_sharing.rs:84`). M3 changes when a monitor looks, not how: a look stays one classified linearizable read through M1's outage policy.

**What looks today, and what it reads**

| Monitor | Interval | Tables one look reads | Time terms |
|---|---|---|---|
| Source listener switch (`sharing.rs:766`) | 1 s | `settings` | none |
| Source body (`shared_library.rs:779`) | 1 s | `settings`, `sharing_exports`, `sharing_identity`, `sharing_export_libraries`, `item_identity_watermark`, `libraries`, `items`, `files`; floor via M2 | none |
| Receiver connection (`shared_receiver_playback.rs:1300`) | 1 s | authority: `tokens`, `users`, `settings`, `sharing_imports`, `sharing_viewers`, `sharing_assignments`, `sharing_identity`; binding and delivery: `sharing_relay_upstream`, `media_sessions`, `job_leases`, `media_session_requests`, `media_playback_pointers`, `sharing_delivery_grants` | session lease, delivery-grant deadline, token idle |
| Receiver owner loops (`:1888`, `:2154`) | 10 s | renewal writes plus one full look | lease |

The owner loops are the lease's heartbeat and keep their 10 s interval and full look; M3 does not touch them, and the acceptance line counts monitors only.

**The wake primitive (vendored Hiqlite, an extension of patch 27, the applied mark).** Only the writer thread sees remote applies, so the primitive belongs there.
- The pending mask widens from `AtomicU8` to `AtomicU16` (`applied_mark.rs:46`, `:74-94`, `:124`), and `APPLIED_MARK_MAX_TABLES` rises from 8 to 16. With 13 tables an 8-bit mask aliases slots 8–12 onto 0–4 in release. A test registers a slot at index ≥ 8 and proves it moves alone.
- A registered table may be **delete-only**: the update hook's action (`writer.rs:289`) decides whether the row change bumps the slot. `tokens` registers delete-only, because every active device rewrites its token every 60 s (`auth.rs:76`, `store/hiqlite.rs:6170`); token idle expiry is a time term instead.
- `AppliedMark` gains one `tokio::sync::Notify` and an **interest count per slot** for the watches alive (a count, so one watch dropping never clears a slot another still holds). `publish` bumps revisions first, then notifies only when a bumped slot has a nonzero interest count, so `media_sessions` renewals cost the writer thread one mask test. `begin_replacement` and `complete` always notify.
- `AppliedMarkHandle::watch(slots) -> AppliedWatch` captures the slots' revisions and the generation. `AppliedWatch::changed()` creates its `Notified` before comparing (`notify_waiters` stores no permit), and returns once a slot or the generation moved. Dropping the watch decrements its slots' counts.

**Exposed through `AppliedView`, not called from sharing.** `AppliedView` has three modes (`applied_view.rs:834-847`, `:868-870`). Only the replicated mode has a mark; `Standalone` (SQLite) and `RemoteAuthority` do not. `AppliedView::sharing_watch(tables)` returns either a mark watch or a 1 s interval, and also falls back to the 1 s interval while `generation` is odd (a failed replacement stays odd, `applied_mark.rs:173-185`). Markless stores therefore behave exactly as today.

**Registration.** The tables monitors wait on join the registered set, giving 13 slots:

| Registered set | Tables |
|---|---|
| Already registered | `settings`, `media_sessions` |
| New | `sharing_exports`, `sharing_export_libraries`, `sharing_identity`, `item_identity_watermark`, `sharing_catalogue_revisions`, `libraries`, `files`, `tokens` (delete-only), `sharing_imports`, `sharing_viewers`, `sharing_assignments` |

- `items` is **not** registered. The watermark moves only on `AFTER INSERT ON items` (`item_identity_schema.sql:13-15`), so it would not catch removals. `sharing_catalogue_revisions` is bumped on item delete, library move and kind change (`sharing_catalogue_schema.sql:17-27`). Those triggers are suppressed while `importing=1`; a look then fails closed, and the flip moves the watermark slot.
- `files` has no such trigger and stays registered.
- `settings` and `media_sessions` remain the only `STAMPED` views. The planning view's per-table metric (`applied_view.rs:1552`) iterates `STAMPED`, not `ALL`, so registering `files` adds no series. The comments that say production "registers exactly" the stamped set (`:79-81`, `:86-89`, `:98-100`) are corrected.

**Per monitor.** Every monitor follows the same rule. It arms the watch **before** each look: before the admission look for the Source body (`shared_library.rs:765`), and the watch is moved into the monitor. It re-arms before every re-look. It re-looks at most once a second after a wake: next look = max(wake, last look + 1 s).
- **Source listener:** waits on `settings` and serving loss.
- **Source body:**
  - Waits on `settings`, `sharing_exports`, `sharing_identity`, `sharing_export_libraries`, `item_identity_watermark`, `sharing_catalogue_revisions`, `libraries`, `files` and serving loss.
  - The member floor (M2) is no longer re-observed on an idle stream. Damage to item identity moves the watched rows; a floor that lapses without a row change is caught at the next look. This is a deliberate change from M1, which ended an idle stream after a persistent not-ready floor.
- **Receiver connection:**
  - **Waits on:** `tokens` (deletes), `settings`, `sharing_imports`, `sharing_viewers`, `sharing_assignments`, `sharing_identity` and serving loss.
  - **Wakes at:** the earlier of the lease and the original login's expiry, plus 1 ms, because the comparisons are strict `>`. The delivery-grant deadline needs no term of its own: it is `min(observed + 30 s, lease, login)` (`sharing_receiver_sessions.rs:66-79`) and is renewed with the lease.
  - **Where the deadlines come from:** the lease is read from the owner's in-memory state at every wait (`delivery_deadline_ms`), so a renewal moves it; a deadline wake re-checks and looks only if the term has really lapsed. A value from the last look would go stale at each 10 s renewal.
  - **New accessors:** the look returns the delivery deadline, which is read and discarded today (`:940-946`). An accessor exposes `login_expires_at_s` (`sharing_receiver_sessions.rs:48`).
  - **Not watched:** `users`. The predicate uses it only for existence (`:667`), and deleting a user cascades to `tokens`, `sharing_viewers` and `sharing_assignments`, all of which are watched.
  - **Also not watched:** the relay and delivery tables, which every 10 s renewal writes. A binding taken or ended elsewhere is seen by this owner's next renewal, which fails and retires through M1, and by the lease deadline. Nothing calls `revoke_receiver_delivery` outside retirement.
- **Unknown spans** keep M1's 1 s re-look until a definite answer.

**Invariant, confirmed.** While the serving fence is ready, revocation latency becomes local apply lag, and readiness bounds it in time:
- Ready needs zero apply lag or an eligible serving proof (`serving_fence.rs:1359-1375`).
- That proof is a quorum confirmation with `applied_index >= committed_index`, kept inside its own 1 s lease (`QUORUM_WATERMARK_LEASE`) and never restamped (`migration.rs:5593`, `:6250-6316`, `:6452-6466`).
- So a ready node has applied a commit index that a quorum confirmed within the last second. Otherwise the fence drops and the monitors' serving-loss arm fires.
- M3 adds no sharing-side timer.
- The anchor precedes the sample: `started_nanos` is taken before `source.sample()` (`migration.rs:7076`, `:7083`), the sample is a fresh leader read index (`get_read_log_id`, `client/mgmt.rs:170`), and `publish_watermark` refuses a sample whose anchor is already past its lease.

**Coverage proofs (prerequisites, each its own commit).**
1. `applied_view_tables_never_escape_the_update_hook` (`placeholder_census.rs:3445`) moves from `APPLIED_VIEW_TABLES` to the full registered set. The unqualified deletes in `validation_reset_contract_state` (`store/hiqlite.rs:5871-5893`) gain `WHERE true`, as `:5835` already has.
2. The census is textual and covers this binary only (`:3449-3451`), so a behavioural store-contract test backs it up. For each registered table, it runs one production write path and a bare `DELETE FROM t`, and asserts that exactly that slot and its cascade children move (or, for `tokens`, that only the delete moves it).
3. Every library has a `sharing_catalogue_revisions` row: the delete trigger is an `UPDATE` (`sharing_catalogue_schema.sql:17-19`) and moves nothing without one. Rows come from the insert trigger (`:13-16`) and the import backfill (`hiqlite_import.rs:2675`). A contract test pins this invariant.
4. Cascade children get a contract test that a cascaded delete moves the child's slot: `tokens`←`users` (`hiqlite.rs:339`), `sharing_viewers`←`users`, `sharing_assignments`←`sharing_imports`/`sharing_viewers`, and `sharing_export_libraries` (`sharing_schema.sql:34,62,66-68`).
5. The interest counts: a `media_sessions` renewal with only sharing watches live does not notify.

**Backstop: none.** The coverage proofs and the markless fallback are the defence; a periodic re-look would hide their failure. The receiver keeps the owner's 10 s look regardless.

**Known harmless costs.** A rolled-back transaction still bumps its slots, which is a spurious wake. `note_row_change` compares table names per row; with 13 names this is replaced by a match on the hook's table pointer only if profiling shows it.

## Acceptance (from the audit)

- `sharing_read` ≤ 2 per request, and 0 per idle stream-second from the monitors after M3 (the receiver owner's 10 s look is the lease heartbeat and is excluded).
- A 3 s leaderless window does not end a shared session (M1).
- An affirmative grant revocation ends the stream within one applied change (M3), and at once on the next look before then.

## Landing

M1 lands on `main` on its own: it is one complete fix for a live user failure. M2 and M3 then go through `effort/sharing-authority-a6` per [AGENTS.md](../../AGENTS.md) "Large efforts": M1 and M2 share `shared_library.rs` and `serving_fence.rs`, so the no-overlap exception does not apply. Each task gets its own adversarial review of the code and the effort development gate; the effort reaches `main` through the promotion gate and a qualification receipt.
