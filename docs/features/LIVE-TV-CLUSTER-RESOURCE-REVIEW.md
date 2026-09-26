# Live TV resource review — challenge the distributed ownership contract

**Status:** revised design accepted; implementation evidence outstanding · **Reviewed:**
2026-09-25 · **Reviewer:** independent adversarial agent · **Scope:** design,
source inspection and failure interleavings; no implementation acceptance

Companion to the [implementation contract](LIVE-TV-CLUSTER-RESOURCE-IMPLEMENTATION.md).
This records objections to the proposed replacement for permanent device
ownership, their concrete failure cases, and the checks that close them.
The implementation plan owns the final decisions; this file preserves the
initial findings and appends their subsequent disposition.

## 1. Baseline and review method

Source baseline: `bafeb08766ce057634f3fab0850cdd9e03507a98`, with an existing
dirty working tree. No implementation files were edited. The reviewed plan's
SHA-256 was
`f31633c3cc334f9a83138259b5711d754f4cb97bad8ff7cf11b9702da3912573`.

Read the plan independently against the current
[manager](../../crates/plurxd/src/live_tv.rs),
[public start/recovery routes](../../crates/plurxd/src/http/live_tv.rs),
[DVR worker](../../crates/plurxd/src/live_tv/dvr.rs),
[replicated DVR mutations](../../crates/plurx-core/src/store/hiqlite_dvr.rs),
[coordination API](../../crates/plurx-core/src/cluster/coordination.rs),
[membership guards](../../crates/plurx-core/src/cluster/membership.rs), and
[restart/migration path](../../crates/plurx-core/src/cluster/migration.rs).
The [existing DVR contract](LIVE-TV-DVR-IMPLEMENTATION.md) supplies the
user-visible stop and sharing behavior that this effort must preserve.

The review examined lost replies, delayed starts, stale worker wakeups,
pruning, last-consumer races, recording stop/finalization and old member
restarts. Findings below identify missing or contradictory contracts; they
do not claim a defect has been reproduced in code that does not exist yet.

## 2. Findings requiring a plan revision

### F1 · P1 — pruning can turn a retired start into a new admission

**Contract at issue:** §4.4 promises that a retired ID never resumes, retains
terminal rows for at least 24 h, prunes them thereafter, and makes recovery
of an unknown ID return expired. The start API still accepts client-generated
128-bit IDs. That API cannot distinguish an unseen ID from a pruned ID.
The contract also does not explicitly require retirement of an absent ID to
insert a durable tombstone.

**Counterexample:** A start POST for ID X is delayed before reservation.
The user's next action retires X through another ingress. An implementation
that updates only existing rows acknowledges retire but inserts nothing;
the original POST then reserves and opens. Even with an inserted tombstone,
the same delayed or repeated POST can create new work after its 24 h record
has been pruned. Making the recovery endpoint reject unknown IDs does not
constrain the original POST. Client retry guidance cannot enforce a server
invariant.

**Required fix:** Specify insert-or-terminalize for absent retirement in the
same identity namespace as admission. Reserve tombstone capacity when
admitting starts so retirement is never refused because a quota filled.
Explicitly choose the post-retention guarantee: either retain enough durable
rejection information, or introduce a server-issued admission ticket/epoch
with an enforceable validity window before deleting its detailed record.
If arbitrary legacy IDs remain accepted, document the bounded legacy replay
guarantee; do not promise indefinite rejection for information the server
has discarded. A new ticket must not silently authorize an old retired
operation merely because its detailed row was pruned.

**Validation:** Exercise retire-before-reserve, reserve-before-retire, retire
on an unknown ID with terminal quotas full, replay during retention, and a
start POST after GC. Assert both response and actual tuner-open count through
different ingresses and after SQLite restart/Hiqlite recovery. Test old and
new client protocols separately.

### F2 · P1 — a new recording can join an ingest already closing

**Contract at issue:** §4.1 atomically joins an existing channel and acquires
a capture epoch, but does not define which ingest states accept joins or the
atomic boundary between removing the last sink and draining the ingest.
There are independent ingest and capture leases, without specified rules for
their relationship. Current `close_finished_transports` selects transports
with no live local sinks and later calls `close_transport`; retaining that
local decision after distributing claims creates a race.

**Counterexample:** Recording X is the last sink on channel C. Worker A
observes X finishing and schedules C's closure. Scheduler B commits recording
Y's claim against C while the ingest still appears active. A then cancels C
before Y attaches. Y has a current recording claim but no viable ingest;
the unique-channel row can keep it assigned to that dead transport until
expiry. A related case is an ingest lease expiring while a joined capture
lease remains current: checking only the capture epoch would permit progress
or publication on revoked ingest authority.

**Required fix:** Define a transactional durable consumer relationship and
the last-consumer drain transition. A join may target only explicitly
joinable current states; the same transaction must exclude draining and
expired ingests. Serialize the final detach/transition to draining against
join, and make worker attach recheck the resulting ingest and capture tokens.
Specify renewal and loss propagation: capture authorization must not outlive
the ingest authority for receiving bytes, while finalization of sealed bytes
needs a separate explicit authority rule. If draining wins, Y waits for or
claims a new ingest epoch; it must not attach to a closing one. Stopping X
must not cancel Y when Y's join wins first.

**Validation:** Barrier tests for both orders of last detach versus new join,
ingest renewal versus capture renewal, ingest expiry with current capture
claim, and delayed attach after worker restart. Count GETs and sinks, prove
Y either has a viable ingest or a bounded recoverable outcome, and verify
that stopping one of two recordings leaves the other ingest running.

### F3 · P2 — stop intent must preserve the recording already captured

**Contract at issue:** §6.3 says publication checks stop intent, and the §7
matrix says stop intent prevents claim/publication. The existing DVR contract
deliberately finalizes a stopped recording as `done` or `partial`, retaining
its bytes and attributing the stop to the user. A blanket publication refusal
would change that behavior.

**Counterexample:** A recording has captured thirty minutes. The viewer
presses Stop. The worker closes and seals its own artifact, then attempts
publication with the correct epoch. If `stop_requested_at_ms IS NULL` is part
of the publication fence, its publication is permanently rejected. The
recording disappears from the library or stays unfinished despite a normal
user stop.

**Required fix:** Separate stop-capture intent from delete/suppress-publication
intent. Stop prevents another capture attempt and further ingest, but permits
the authorized finalizer to publish sealed captured bytes as the existing
partial/done result with stop attribution. Explicit delete uses a distinct
durable terminal intent. Define who can finalize after a stopped worker loses
its lease; a replacement finalizer must not need to reopen a tuner.

**Validation:** Stop an active recording, then lose the worker before and
after sealing. Verify available sealed bytes remain publishable with the
correct attribution, no fresh capture starts, and explicit deletion racing
finalization cannot resurrect a library item.

### F4 · P2 — old member restart needs an identified compatibility fence

**Contract at issue:** §8 requires a new capability for joining/serving and
states that old binaries cannot restart into serving authority, but relies
on an unspecified existing fence. The current Live TV capability guards in
membership protect join reservation and staging. A process restarting with
an existing member identity does not necessarily perform either operation.
The existing activation helper also treats absent members as pending until
removed; the proposed offline-owner migration needs to reconcile that rule.

**Counterexample to a join-only implementation:** A is already a member and
has the historical owner settings. It goes offline. B and C enable cluster
mode, after which A restarts its old binary under its existing identity.
If only the new join guard enforces the new capability, A can follow the
legacy owner path without consulting new ingest or artifact claims. Merely
advertising the new capability on B and C does not establish that A is fenced.

**Required fix:** Name the supported rollout mechanism before M5: enforced
protocol/schema transition recognized by predecessor binaries, or removal of
unupgraded absent members with their old identity unable to regain serving
authority. State how the mode transition atomically excludes incompatible
serving membership and how local SQLite downgrade is refused. If a required
underlying fence does not exist, make its implementation a prerequisite;
do not describe it as inherited behavior. This is software serving and
publication fencing, not a demand for physical tuner release or a manual
power-off attestation.

**Validation:** Test existing-member restart as well as fresh join. Include
an absent unupgraded owner, a removed identity, rollback of an upgraded node's
binary, and single-node downgrade. Use the supported predecessor binary or
its exact startup/serving protocol, rather than a new binary merely hiding
its advertised capability. An old hardware socket may remain; other eligible
workers must still be able to use spare device capacity.

## 3. Accepted decisions and optional work

Permanent node ownership is unnecessary. The selected separation between
device capacity, cluster policy, per-ingest responsibility and storage
placement is sound. The review does not require physical fencing to admit
independent work on spare tuners. The plan correctly admits that expiry can
leave an old socket physically open and that HTTP refusal remains possible.

Epoch-specific immutable file paths, excluding unsealed predecessor output,
and indexing only committed artifact manifests address the important
filesystem corruption cases. A same-channel storage-placement conflict is an
explicit accepted tradeoff, not an undisclosed owner restriction. Cross-node
recording relay and general viewer sharing remain optional later work.

The guide's second durable copy and takeover are reasonable protection from
an ingest-node failure. Exact storage format and transfer batching can be
chosen during implementation within the plan's bounds. This review does not
require consensus time to control physical hardware: epoch CAS fences and
the stated synchronized-clock trust model are adequate for the claimed
logical guarantees if the clock-jump tests pass.

The initial SQLite source link pointed to a nonexistent file; the author
corrected it during review to
[sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs). This is a
documentation fix, not an architecture blocker.

## 4. Initial verdict and limits

**Verdict:** revise before calling the contract ready to implement. F1 and
F2 are correctness blockers. F3 and F4 require explicit contract decisions
before the relevant implementation task starts. None requires restoring a
permanent device owner or global physical-fencing ceremony.

This review ran no Rust tests, changed no executable behavior, contacted no
device, and verified no physical HDHomeRun firmware behavior. The plan's
compilation, backend races, mixed-version runs and physical acceptance remain
implementation gates. Document corrections resolve design findings only;
passing source review cannot substitute for those gates.

## 5. Revision verification

Re-reviewed on 2026-09-25 against plan SHA-256
`609c85ebd6cf1d92f69ed26c0d9d48df3fb99acda261e93cf89ef23dc009d4ad`.
The source baseline remains the same. The following accepts written
contracts, not unimplemented behavior or unrun tests.

| Finding | Disposition | Verification and remaining evidence |
|---|---|---|
| F1 · P1 | Resolved in the design | §4.5 creates durable issued intents before returning protocol-4 tickets, rejects absent ticketed starts, reserves terminal capacity, and defines absent retirement. Legacy IDs explicitly retain a bounded 24 h guarantee, with a compact per-user admission block when unknown retirement cannot allocate history. Prove both protocols' actual GET counts across retirement, quota exhaustion and GC. |
| F2 · P1 | Resolved in the design | §4.1 and §6.1 add durable consumers, restrict joinable states, serialize last detach/drain against join, preserve pending demand and renew linked ingest/capture authority atomically. The current channel key explicitly excludes epoch. Prove both race orders, abandoned attachment and ingest-loss propagation. |
| F3 · P2 | Resolved in the design | §6.3 separates stop from delete and introduces independent finalizer authority for already sealed bytes. A finalizer does not reopen the tuner; deletion prevents publication and resurrection. Prove stop before/after sealing, worker loss, stable recording identity and deletion races. |
| F4 · P2 | Resolved as an explicit implementation prerequisite | §8.1 names SQLite/replicated schema compatibility and committed-membership admission, requires upgrade or removal of absent incompatible members, and mandates an actual predecessor-binary restart fixture. If that path is insufficient, a prerequisite fence release is required before mode enablement. The existing fence has not been accepted merely on assertion. |

### 5.1 The re-review found and closed a cross-protocol replay hole

The first revision introduced tickets while leaving request namespaces
unspecified. A pruned ticketed ID submitted to the legacy start API without
its ticket could then be mistaken for a new arbitrary legacy ID. That would
undo the stronger protocol-4 guarantee without touching the recovery endpoint.

The final revision requires protocol-4 IDs to be `v4_` plus 32 lowercase hex
digits, while legacy IDs remain exactly 32 hex digits. Dispatch is enforced
by ID grammar and a durable protocol discriminator, not by a caller's choice
of header or ticket presence. All start, relay and recovery validators change
together. §11.2 explicitly tests stripped-ticket legacy fallback after GC.
This closes the design hole; implementers must preserve the disjoint grammar.

The re-review also requested that current recording-channel uniqueness
exclude epoch. §4.1 now makes epoch a value of the unique current assignment,
with older epochs retained only as history. An index including epoch alone
would not enforce this contract.

### 5.2 Final verdict — ready for implementation with named proof gates

**Verdict:** the revised design is ready to implement in its stated sequence.
No unresolved design blocker remains from this review. Initial findings stay
in §2 as an audit trail; none was waived. The adjacent Activity/reminder
owner gates and stable legacy recording identity are now explicit M3 work.

This acceptance depends on the limits being kept visible: legacy replay
protection is bounded, physical sockets are not fenced by lease expiry,
local-only storage constrains recording recovery, and seamless live-session
migration remains out of scope. No extra tuner-owner election or hardware
attestation is needed to satisfy these contracts.

Before enabling cluster mode, the real backend races and predecessor-binary
restart tests remain mandatory. Before promotion, the exact-candidate gates
and physical device acceptance remain mandatory. No runtime, compilation or
hardware evidence was produced by this documentation review.

**Artifact finalization:** after the accepted re-review, the author updated
only the plan status header and §12 verdict text to reflect that acceptance.
The implementation contracts are unchanged. Final delivered plan SHA-256:
`cd6cb0867c9d5e52cce8beb9b8ed72daf77aa4a0153704cafb92e20ab6dd06e3`.


## 4. Final implementation review — PR #537

One independent adversarial code review examined candidate `3486578c7`
against main `2b6cb21e6`, followed by verification of the corrections in the
same review pass. The reviewer executed no tests.

| Finding | Correction | Disposition |
|---|---|---|
| P1: refused replay could fail an existing valid start | Remove unconditional caller cleanup; workers close their own claims. Check immutable payload digest on advancement. Unspawned pending starts expire and detach their consumers. | Verified by reviewer; regression added. |
| P1: unavailable recording storage aborted unrelated scheduling | Recovery, Stop and end-of-window finalization defer individual rows when storage/claim is unavailable, allowing the remaining DVR tick to proceed. | Verified by reviewer. |
| P1: sidecar failure could publish an unindexable recording | Require exclusive sidecar creation, complete write and fsync before manifest publication; failure retains capture inputs for retry. | Verified by reviewer. |
| P1: stale row snapshot could omit a newer capture attempt | Re-read the recording after finalizer admission, compare its attempt with the claim, and assemble/delete through the claimed epoch. | Verified by reviewer. |
| P2: guide refresh retained its lease after completion | Release the exact latest token after success or ordinary failure. Cancellation retains bounded expiry as fallback. | Verified by reviewer. |

Final verdict: **approve the corrections for the final unit/fast-lane run**.
No remaining blocker was identified in the corrections. This is code-review
evidence; it does not claim runtime, physical tuner, or predecessor-binary
qualification. The PR status page records the final test and merge result.
