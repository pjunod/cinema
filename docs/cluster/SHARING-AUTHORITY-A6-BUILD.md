# Sharing authority through the cluster layer — A6 build plan

**Status:** plan rev 3, after adversarial review and confirmation pass (Fable); sound to build · **Owner:** Claude Opus 5.5, session_01BCWLA21tCtxWeNBWRHWsWn ·
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

- **Wake sources:** a revision change of the sharing grant, import, assignment and switch rows; an image-generation change; serving loss; or `sleep_until(earliest expiry)` for the time terms (lease, grant deadline, token idle). No fixed interval.
- **Prerequisites:**
  - extend the placeholder census to the sharing tables, whose SQL is built with `format!` (`placeholder_census.rs:3443-3452`);
  - confirm the sharing tables are rowid tables updated through qualified statements;
  - a vendored Hiqlite wait API on `AppliedMarkHandle`, which is used only through polled getters today. That is a new patch, upstreamable.
- **Do not wake on `media_sessions`**, whose 10 s lease renewals would wake every monitor.

## Acceptance (from the audit)

- `sharing_read` ≤ 2 per request, and 0 per idle stream-second after M3.
- A 3 s leaderless window does not end a shared session (M1).
- An affirmative grant revocation ends the stream within one applied change (M3), and at once on the next look before then.

## Landing

M1, then M2, then M3, as separate PRs through the train. Each gets its own adversarial review of the code.
