# Peer proofs and artifact hydration

**Status:** open — implementation in progress · **Owner:** Codex peer lane ·
**Executes:** B5 and B3a from [the cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) ·
**Written:** 2026-11-19 · **Base:** `09d4d9502`

## 1. Objective

Use the cluster-owned call path for bounded artifact racing, then generalize
peer and token proof reuse without confusing cached authentication with fresh
mutation authority. This task does not change quality placement, timing
policy, source-reader capacity, or the separately owned B3/C1 read authority.

## 2. Ownership

The peer lane owns `cluster.rs::calls`, `fragment_index_cluster.rs`,
`copy_index_v3.rs`, `subtitle_source.rs`, and their focused regressions.
Later proof work owns core membership authorizers and `http/extract.rs` with
its existing revocation lifecycle. The timing lane owns timing constants;
this lane preserves the existing eight-second artifact budget and exposes
no new tuning setting. Root owns quality placement and integration.

## 3. Hydration contract

At most four holder requests run concurrently. All requests in one lookup
share the existing eight-second absolute deadline; a late holder does not
restart the budget. Futures are owned by the lookup, not detached tasks.
Returning a verified artifact or cancelling the lookup drops every losing
future. The first successful HTTP response is only a candidate: existing
size, digest, source, pipeline and representation checks still decide it.
Only verified bytes may be installed or published as a new local holder.

Copy-index absence remains three-valued. Only the exact holder response
`404` with `ABSENT_BODY` removes a location and reduces the possible-holder
count. Timeouts, unreachable members, route refusal, malformed bytes and
unknown responses remain `Unavailable`, never evidence of `NoHolder`.
Subtitle stale-publication cleanup retains its existing exact verdict.

## 4. Proof contract

The existing per-sender read-authority cache has a one-second lease and
single-flight refresh. Any generalization must distinguish voter/member
roles, retain signature, nonce and rate admission, and leave the clock
route's exact applied-roster proof intact. Fresh membership mutation and
credential-revocation claim admission remain fresh authority operations.

A token proof must carry its audience, user, exact policy/expiry boundary,
source identity and revocation generation. `TokenAuthentication` currently
returns only `User`; that is insufficient to construct an expiry-preserving
cache. Inventory every credential/policy writer before admitting general
cache hits. Existing all-member (learners included) claim-bound revocation,
ambiguous-write fences and stale authentication-ticket rejection remain the
invalidation mechanism. An applied replica alone grants no token authority.

## 5. Validation and delivery

Establish Rust 1.97.1 in an isolated source-only compile slot before Rust
edits. Use four compiler jobs and eight-MiB test stacks. The old global
Cargo target is not evidence because it mixed independent source trees.

Focused regressions must show a stalled first holder cannot delay a later
valid holder, corrupt fast answers cannot win, deadline/cancellation bounds
hold, and exact absence remains distinct from unknown. Run the actual
fragment/copy/subtitle fixtures, not only helper tests. Proof changes require
negative expiry, audience, revocation, stale-ticket, role and source controls.
Record exact source and execution receipts. Final integration requires the
normal effort gate and current-main promotion; no fleet deployment occurs
in this lane.

## 6. Implementation progress

Bounded racing is implemented for fragment indexes, copy-index v3 and
subtitle representations. Candidate validation and stale-row cleanup live
inside each candidate future, so cleanup on an invalid fast response cannot
hold up a valid peer. Installation and publication remain single-winner.
Losing candidate futures, including best-effort exact stale-row deletions,
are cancelled with the lookup; these deletions are idempotent catalogue
cleanup, not authority or lease mutations.

The pinned macOS daemon baseline at `259e1f06a` fails on the two inherited
Linux-only `vodencode::dv_runtime` references in `state.rs`. No passing daemon
build or regression execution is claimed. Linux compile qualification is
being established. Peer-authorizer reuse, outcome health and token-proof
caching remain unimplemented; this bounded hydration change does not close
B5 or B3a by itself.

## 7. General token proof admission

`UserStore::authenticate_token_proof_for` will default to a normal
Store-backed authentication with no reusable witness. Hiqlite returns a
committed token/policy witness from its existing joint statement. SQLite
stays uncached because local Store writers do not carry the replicated
rollback guards and have no quorum round to save. Its wall-clock expiry is the earlier of the policy's exact idle
expiry and the next activity-refresh boundary (at most sixty seconds).
Monotonic cache age also caps that lifetime, and a backwards wall clock
refuses reuse. Playback handoff tokens retain their own path and no cache.
Audience is part of the key; native and compatibility proofs never cross.

The existing cache-state mutex/generation/revocation guards will own a
separate bounded general-token map. Every global, digest, user, ambiguity,
capability-loss, timeout and peer begin/end invalidation applies to both
maps. Only fresh Store authentication publishes a recovery-admin proof;
a general-token cache hit cannot renew its five-minute lifetime.

The running capability becomes `cache_admin_revocation_v5`. A mixed or
rolled-back roster cannot admit general cache hits. Policy writes to the
three `auth.token_*` settings must use a global claim-bound revocation and
a transaction-local credential intent. Replicated INSERT/UPDATE/DELETE
triggers protect those keys after permanent guard activation, preventing an
older binary's plain settings SQL from bypassing invalidation. The existing
activation/intent/lease tables are reused; no table-watch width is added.
The ordinary settings API refuses guarded policy mutations once active;
the explicit policy API checks the exact live claim in each statement.
SQLite uses the local revocation guard and atomic settings transaction.

Policy writers are the settings save in `http/system.rs` and the initial
clock in `http/users.rs`; the latter initializes an absent clock before
admission and cannot revoke an existing active cache. Tests must cover a
policy tightening, stale authentication publication, mixed v4/v5 members,
legacy raw policy SQL, audience separation and expiry at the exact boundary.

The initial expiry clock now reads an existing setting before attempting
its atomic seed. A no-op INSERT still runs BEFORE INSERT guards; avoiding
that write preserves ordinary restarts without weakening the guard against
`INSERT OR REPLACE`. A missing clock after permanent activation is refused
rather than seeded around live proofs. User-role promotion also takes the
existing user revocation bracket, so cached viewer roles are invalidated.

## 8. Peer authority reuse

Generalize the existing per-sender one-second single-flight cache to two
independent leases: committed member and committed voter. A member proof
never satisfies a voter check. Every signed request/response still verifies
its exact bytes; request nonce replay and rate admission remain separate.
The clock route keeps its existing fresh, exact applied-roster algorithm;
membership mutation authorization and revocation claims remain fresh.

A reused lease is bounded by the original heartbeat's reachable-through
wall time and a monotonic expiry anchored before the authority read. Its
stamp binds the exact committed membership log (including ABA), local image
generation, and the nodes/removals/activity-key table revisions. Unknown
local stamps fall back to the original fresh query. Missing registered table
slots fall back to the applied index, which is conservative but less cheap.
A moved stamp does not publish or reuse a proof. Key eviction removes both
role leases through the existing per-sender ownership map.

Transport errors already have a typed `PeerTransportError` and media-session
control has typed lifecycle errors; retain those paths rather than replacing
them with strings. The remaining admission/refusal classification and health
observations must describe call outcomes without treating an unauthenticated
response, a timeout or cancellation as committed removal or credential
revocation. Health can rank candidate attempts; it cannot grant authority.
