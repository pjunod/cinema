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
