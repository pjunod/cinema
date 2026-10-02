# Clock enforcement — consume proved observations before acquiring authority

**Status:** open — E0 pure policy in preparation; active consumers unclaimed;
measurement evidence pending · **Executes:** K-06 enforcement handoff ·
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

**2026-10-01 coordinator ruling — prepare E0 now, integrate active consumers
after evidence.** Under Paul's delegated routine-decision authority and newer
effort/main-at-end workflow, E0 pure policy may be claimed/developed now on
`codex/k06-pure-clock-policy`, owner `gpt-6.1-sol`, session
`agent:/root/s14_resume_sol61`. This supersedes §1's before-claim prohibition
only for preparatory source. Active production refusal consumers still require
successful identified measurement evidence before effort integration. A
separately identified measurement-only current-effort artifact observed in an
owned isolated four-node LAN lab is eligible for that safety bar, not a main
release or production-fleet qualification. Keep one-hour idle and 60-second
actual network load, 250/2,000 ms bounds, auth windows, no-switch/no-gate,
original failures and 24-hour/drill final acceptance intact. No deployment,
clock step or lab launch is authorized by this source ruling.

E0 owns only core `cluster/clock.rs`: typed acquisition/revalidation policy
with caller-bound time, exact proved NoPeers, generation/freshness/continuity
refusals, and completed-positive-round readiness facts. No membership,
media-session, router or Developer consumer is connected; exported refusal
metrics remain measurement-only zero. Target exclusion is deferred until its
actual durable-fence and target-applied/unreachable proof can be represented
alongside the real removal consumer; boolean assertions are not authority.
The following consumer milestone must preserve commit-unknown takeover
reconciliation even when a later clock ticket refuses new acquisition.

**2026-10-01 private consumer preparation:** coordinator `agent:/root`
owns `codex/k06-consumer-preparation-20261001`, based on actual effort
`11ad24fb` plus the additive current-main preservation composition `4fef53b9`.
Under Paul's delegated routine-decision instruction, preparation now extends
to E1 consumer source while the owned measurement window is unavailable.
This supersedes the E0-only preparation limit, not the successful-measurement
requirement before enforcement enters the effort. No push, rollout, production
clock step or qualification is implied. Keep this source private until the
identified receipt exists and the complete consumer scope has one review,
focused evidence and the current gate.

The first working slice connects completed-positive-round readiness to
`/readyz` and the existing Store-free operations projection. Maintenance,
quorum and Store failures retain precedence; Unknown is not positive clock
violation, scraping never counts rounds, and liveness is unchanged. Typed
consumer admission counters have exactly three decisions by four refusal
causes; pure policy inspections and scrapes remain uncounted. Takeover,
membership, fenced-target removal and the final enforcing Developer facts
are still pending, so this is not complete enforcement or a merge candidate.

**2026-10-01 original-time takeover preparation:** root consulted an Astra
design session (not the formal PR review) before choosing the detached-task
boundary. The private implementation carries an opaque, exact-`Arc`-bound
owned acquisition ticket from before preparation into the supervisor. It
never reconstructs authority from measurement generations or reacquires a
fresh timestamp after preparation. The expiry scan also uses one original
ticket timestamp and discards a page invalidated during its Store await,
without advancing its keyset cursor.

The immutable takeover proposal keeps `now_ms = ticket.now_ms()` and
`lease_expires_at_ms = ticket.now_ms() + 24_000`; its monotonic expiry starts
immediately before that same acquisition, not after worker creation. A
synchronous final revalidation precedes only the initial CAS. Refusal stops
the same provisional worker and does not enter commit-unknown reconciliation.
After submission, exact replay/read, pinning, bootstrap renewal, adoption,
ordinary renewal, serving and self-fencing remain ungated.

This preserves the original contract and constants, **not unchanged
worst-case recovery availability**: at most eight seconds of preparation
spends the original 24-second lease, leaving approximately 16 seconds against
the shipped 13-second publication-runway floor. Three one-second Store
windows can spend nearly all remaining margin. Existing runway checks safely
refuse late publication; successful bootstrap renewal still grants its
existing fresh 24-second lease before adoption. Adding preparation time to
expiry or minting a post-preparation replacement ticket would silently widen
or replace the fixed proposal, so neither is implemented. Membership and
fenced-target removal still require their complete separate boundary audit.
This is private source preparation; current compiler/focused results and
measurement evidence are not inferred from the decision.

**2026-10-02 private membership preparation:** the same root-owned branch
captures pure opaque admission results before the first awaited reads in
`redeem_for_role`, `promote_learner` and `activate_learner_protocol`. The
[core guard](../../crates/plurx-core/src/cluster/clock.rs)'s `admit_for`
consumes that original result only when the authoritative lifecycle reads
identify new authority. It never reacquires a ticket after preparation:
an original Unknown/Offset refusal stays refused even if observation has
recovered, and an originally safe ticket must still match the exact guard,
both generations and current policy. Only an actual refusal increments the
bounded `membership_change` counter; unused idempotent inspections do not.

Redemption rechecks immediately before its new staging transaction; the
caller-bound original time is the only admission timestamp bound into SQL.
Exact published-node/HTTP-origin repair and post-submission error
reconciliation stay ungated. Promotion rechecks before a new durable intent
and before a new HTTP proposal, retains original `started_at`, and may clear
only this invocation's newly created, still-unsubmitted intent on refusal.
Earlier ambiguous intents and committed-voter role reconciliation remain.
Repeating the HTTP promotion request constructs a proposal against the
leader's current voter set, not an immutable replay of the old set, so a
fresh submission still requires the original proof; read-only outcome
reconciliation does not. Protocol activation preserves already-active
idempotence, binds the original absence cutoff and rechecks before its
compare-and-swap. Existing SQL capability/role/range fences are unchanged.

The typed public refusal is `cluster_clock_unbounded` with HTTP503; no
setting, deployment or switch is introduced. New focused development IDs
are `prepared_admission_never_replaces_original_refusal_time_or_guard` and
`change_refuses_unbounded_clock`. The latter combines actual guarded
protocol SQL with explicit source-wiring checks; it is not a real Raft/fleet
observation. Their actual outcomes must be recorded separately, never
inferred from the implementation or an earlier compiler snapshot.

**2026-10-02 private finalization preparation:** `finalize_for_role` now
captures before its awaited token read and passes the opaque original result
to `dispatch_clocked_join_finalization`. That production dispatcher retains
the existing committed-role validation and completed retry short circuit.
Only a still-redeeming token's new lifecycle publication consumes/revalidates
the original proof; the original time is bound into the unchanged guarded
token SQL. An initial refusal cannot be replaced by later healthy evidence,
and changed evidence during the role read refuses before any CAS. Once the
CAS is submitted, its result is handled by the existing lifecycle consumer;
clock admission does not reinterpret its outcome or block an already-redeemed
retry. No token TTL, role validation or token/node predicate is weakened.

One genuinely new focused method,
`finalization_preserves_clock_ticket_and_completed_retry`, passed once on
the private working source. It invokes the actual production dispatcher and
token SQL for both admitted roles, covering completed retries, Unknown-to-
healthy transitions, post-read invalidation, incorrect committed role,
original-time publication and a changed token owner. This remains SQLite
primitive/dispatcher evidence, not a real multi-node Raft or fleet run.
The earlier nine focused successes retain their original source attribution
and are not replayed. Exact current compiler/hook receipts are separate.

This slice does **not** finish membership enforcement. Target-excluded
durable removal and the vendored actual Raft admission
boundaries still need implementation/audit. In particular,
[management.rs](../../vendor/hiqlite/src/network/management.rs)'s
`add_learner` and `become_member` call Raft below the application manager;
guarding only application redemption or the outbound promotion request
cannot close their leader-side awaited preparation window. A later complete
implementation must bind the same node-local guard there without introducing
a second guard, startup bypass or upward dependency from Hiqlite into core.
No private enforcing source enters the effort before the unchanged identified
measurement receipt; the original removal, replay and qualification scope
remains open.

### E0 interfaces — local policy without an irreversible operation

The 2026-10-01 review correction invalidates current evidence and advances
its generation inside the publication lock when a matching-generation round
omits or adds roster peers. Acquisition cannot race a later caller cleanup.
An obsolete-generation round is rejected without overwriting newer evidence.
The focused regression is
`acquisition_current_missing_peer_invalidates_atomically_but_stale_round_does_not`.

The [shared core handle](../../crates/plurx-core/src/cluster/clock.rs) offers
`acquire() -> Result<ClockAcquisitionTicket<'_>, ClockRefusal>` and
`revalidate(&ClockAcquisitionTicket<'_>) -> Result<(), ClockRefusal>`.
The acquisition ticket is opaque, borrowed from and bound to that exact
handle, not a constructible pair of equal-looking generations. Its `now_ms()`
is the original caller-bound time; revalidation never replaces it. Both
reads synchronously check local continuity, evidence expiry and the serialized
policy state. Typed refusals distinguish Unknown, Offset, LocalDiscontinuity
and GenerationChanged. The existing constructible `ClockDecisionTicket`
continues to serve measurement, not admission authority.

Replicated NoPeers requires an installed exact empty committed roster and
expires at the same 25-second monotonic watchdog as samples. Standalone
provenance is fixed by handle construction and does not masquerade as an
unfinished replicated discovery. Future-dated, expired or arithmetically
invalid samples cannot authorize acquisition. `ClockSnapshot.readiness`
contains a passive `is_unbounded()` fact after two completed positive
violating rounds; reads never count rounds, and rejected/Unknown/healthy
rounds, roster changes, expiry, failure and discontinuity reset the streak.
No `/readyz` consequence or nonzero production-refusal counter is connected.

The E1/E2 consumer audit must still bind this handle to actual current
membership and revalidate at each final irreversible boundary; a pure local
ticket cannot prove a durable CAS outcome or a target-removal fence. Do not
drop commit-unknown reconciliation or synthesize target proof from booleans.

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
