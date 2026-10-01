# Clock enforcement — consume proved observations before acquiring authority

**Status:** open — owner unclaimed; runtime not started; measurement evidence
pending · **Executes:** K-06 enforcement handoff ·
**Written:** 2026-09-30 · **Source baseline:** effort `f319fa779`.

Companion to [the accepted design](CLOCK-SKEW-GUARD-DESIGN.md) and the
separately owned [measurement release](CLOCK-SKEW-MEASUREMENT-IMPLEMENTATION.md).
Read those contracts before claiming this separate implementation PR. K-06
remains open on the [workboard](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md)
until runtime and acceptance are complete; a Python design model is not an
enforcing binary.

## 1. Claim boundary — identified measurement evidence first

**Future owner/session:** unclaimed. **Implementation PR:** none. The owner
must identify the merged measurement release and its accepted design §5.5
fleet receipt before claiming enforcement. An unmerged combined branch cannot
supply the required release separation.

Paul's 2026-09-30 clarification, recorded in measurement §1, requires a switch
only where manual on/off is meaningful. K-06 retains its accepted no-switch
contract, automatic observation, and separate enforcement release. Developer
facts remain read-only and advisory. Enforcement still waits for the merged
measurement release and its identified fleet receipt; that substantive evidence
dependency is unchanged. No rollout or clock-step authorization is implied.

## 2. Current entry points — inspect all irreversible boundaries again

At `f319fa779`, these are existing consumers to audit, not clock enforcement.

| Surface | Current symbol and required audit |
|---|---|
| Expiry/takeover | [media_sessions.rs](../../crates/plurxd/src/media_sessions.rs): expiry scan calls `expired_media_sessions(now_ms, ...)`; takeover store seams call `claim_media_session_takeover`. Trace each intervening await, claim and publication before placing generation rechecks. Keep own renewal/serving/self-fencing unchanged. |
| Membership | [membership.rs](../../crates/plurx-core/src/cluster/membership.rs): `redeem`, `redeem_learner`, `finalize`, `finalize_learner`, `promote_learner`, `remove_node`, `leave_node` and `wait_for_removal_fence`. Re-census admission/protocol-activation and every durable intent/Raft proposal; one entry guard cannot cover awaits. |
| Readiness | [http/mod.rs](../../crates/plurxd/src/http/mod.rs): `evaluate_readiness` uses cached membership/serving state for quorum-managed nodes. A clock consequence must consume the shared local snapshot, not add a Store request. |
| Replicated time | [replicated.rs](../../crates/plurx-core/src/store/replicated.rs): existing SQL validator rejects clock/random identifiers. Session/membership SQL lives in their current store modules; caller-bound time must remain deterministic across replay. |

The measurement owner supplies the actual shared handle and transport modules;
update these entry points against that merged source before implementation.

## 3. Milestones — preserve acquisition and safe reduction separately

**M0 — evidence-bound policy wiring.** Apply accepted design §§3.3–3.5/5.3
without changing the 2,000 ms bound, auth freshness or lease constants.
NoPeers requires actual exact-roster proof; incomplete discovery is not a
standalone exemption. Takeover/expiry/acquisition require safe upper bounds
and synchronous local continuity. A serialized ticket includes clock/state
generations and the caller-bound now; revalidate after awaits immediately
before each irreversible CAS, intent or proposal. Changed roster, failed
round, expiry, local/common-mode step or generation mismatch refuses the
operation. No fresh independent wall-clock read may replace the ticket's now.

**M1 — fenced-target removal and local readiness.** Preserve every shipped
fence/barrier/tombstone/leader/quorum rule. Only the removal target may be
excluded, after durable fence and target-applied or authoritative-unreachable
proof; unknown survivors, lost quorum or changed generation still refuse.
After a local step require design §3.5's monotonic stabilization or fresh
post-generation heartbeat before wall-age reachability is trusted. Apply the
design's two-positive-violating-round readiness rule; Incomplete stays ready
under the accepted protocol. Keep renewal, admitted bodies and self-fencing
available. Any policy adjustment from §1's ruling requires a dated explicit
contract update and its own regressions before code, not an implicit choice.

**M2 — deterministic replay and operational acceptance.** Extend the positive
caller-bound SQL/replay proofs named in design §5.4; do not relocate lease
stamping to the leader, pass clock evidence into SQL or evaluate time per
voter. Retain bounded refusal/discontinuity metrics and advisory state. Then
qualify this exact enforcing binary for the authorized lab protocol below.

## 4. Acceptance — focused proofs precede any controlled drill

Use the pinned compile loop and design §5.3's named takeover/readiness and
`hiqlite-store` membership/removal regressions, plus §5.4's replicated-clock
tests. Prove generation races and common-mode steps at actual call sites;
the design model alone cannot prove runtime wiring. Canonical Regression-Test
fields, sole review, normal hook and exact-head Effort gate belong to the
owning PR under the [development pipeline](../DEVELOPMENT_PIPELINE.md).

Operational acceptance is design §5.5: 24 hours of healthy flat refusals,
then the separately authorized disposable-lab 15-second clock-step drill,
including a single node and all-voter common-mode step. Before executing,
name the operator, source/build/image, host set, recovery procedure and
explicit operational approval. This plan grants none. Retain observations
that no session changes owner, dangerous actions refuse, readiness follows
the accepted two-round rule and recovery occurs after discipline returns.
Missing/failed observations remain failures; NTP or quorum point samples are
not replacements. Rollback follows the resolved §1 contract and reviewed
release procedure. Close K-06 only after both owned releases and their actual
acceptance receipts exist.
