# Playback control rewrite — project status

**Updated:** 2026-08-28
**Merged baseline:** `origin/main` at `253f3200` (through PR #638)
**Current work:** first actor-owned prepublication recovery cut in
[#636](https://github.com/pjunod/plurx/pull/636) on branch
`codex/playback-control-m4-prepublication` in the disposable clone at
`/private/tmp/plurx-playback-control-clone`. Runtime repair `6e01bbc3`,
restart-drain repair `07bb95bc`, focused migration/fixture repair `baba7c20`,
lint repair `355d3309`, and Hiqlite placeholder-order repair `4da3bbde` now
implement an explicit three-phase activation protocol. Their regression
evidence is closed through `44947095`; hosted-preflight inventory reconciliation
`c4a1d96d` updates sixteen reviewed exact-count sentinels changed by those
repairs:
Prepare commits only a `BLOCKED` route and a starting claim; Confirm atomically
changes that route to ready or to a finite predecessor boundary while leaving
the claim starting; Publish resolves the claim only for the exact current,
live, ready route; and Abandon atomically wins only while the claim is still
starting. SQLite v36 and Hiqlite v18 install the same claim-transition trigger,
including upgrades from already-v17 clusters. Finite handoffs remain renewable
and takeover-safe through the 372-second response-lifetime boundary, then
retain exactly one ready-retry renewal. Remote `START`/`ACTIVATE`, serving
authority, detached worker/replacement ownership, takeover request ownership,
and resolved replay freshness are all explicit. Rejected exact head
`032292a159513e51f13bab5db1d6081c46b0cc5c` exposed that detached remote
`START` did not retain restart-drain admission after handler cancellation;
`07bb95bc` moves that guard into the detached task through worker/watcher
handoff. Three independent review lanes approve every repaired source delta
with no remaining P0–P3 finding. All three also approve the exact inventory
repair with no remaining P0–P3 finding. The synchronized documentation tip is
the final exact-head review candidate before push.

The one allowed full local unit invocation was consumed before tests while
compiling Store code. The directly affected rerun then executed 641 tests:
606 passed initially, five deterministic inventory/high-water failures passed
after exact repairs, and all 30 socket fixtures passed by exact name with
unrestricted loopback binds. The aggregate cluster gate found two stale
historical migration fixtures; `411bf818` reconstructs their exact v10-v12
schemas and `4d38a084` proves the v14 acknowledgement objects and v16/v17
columns at every completed upgrade. Both failed names pass by exact name.
The repaired tip then passed the activation Store contract, five exact daemon
activation/replay tests, two SQLite migration tests, two Hiqlite schema tests,
and the exact placeholder-order invariant. The first required cluster run
passed every vendor WAL/Hiqlite probe and 61/67 replicated Store contracts;
the six failures all identified first-appearance placeholder corruption in
Hiqlite media-session SQL. `4da3bbde` repaired the four reached statement
shapes plus one latent Abandon binding, and all six failed three-voter names
passed on exact rerun. The complete cluster harness, seven real-daemon activation tests,
and two Activity tests pass. Formatting, workspace Clippy, validation catalog,
history, 123 operations contracts, 52 benchmark checks, and the web policy
gate also pass. Every local result is therefore accounted green without a
second broad-suite run. Hosted run `33226975369` stopped in fast preflight on
sixteen stale ownership-inventory counts before every downstream job. After
unanimous review, the two exact failed methods pass 2/2, `make
validation-lint` passes, and the complete hosted-equivalent Python validation
catalog passes 68/68. The PR body pins each immutable review candidate,
avoiding an impossible self-reference from a commit to its own hash.
Hosted rerun `33227941555` then stopped solely because the docs-only candidate
subject matched the corrective-history classifier. Unanimously reviewed mapping
`9471d129` records that `9ee007a8` changed documentation only; the exact
`make history-check` rerun passes with 1,100 corrective commits accounted.
Hosted run `33228540108` then passed validation scope, mobile version, fast
policy/contract preflight, WAL recovery, daemon contracts, and the complete
replicated Store/topology lane. Its final fast-Rust job exposed two stale Core
contracts, two nondeterministic fixture assumptions, and two playlist tests
whose obsolete barrier wait prevented completion; the job was cancelled at
its 30-minute bound before libtest could print the latter target's summary.
The reviewed repair now classifies all 48 SQLite transaction owners, checks
the actual read-only Hiqlite replay shape, rendezvouses playlist races at the
actor observation boundary, exercises software admission without a nonexistent
media source, publishes VOD fixture claims through the complete three-phase
contract, and isolates fair terminal eviction from paused-time Store probes.
All six exact affected tests pass. Three independent repair reviews report no
remaining P0–P3 finding; only immutable-tip review, the hosted rerun, and merge
remain.

The post-rejection work is assembled in three tracks:

- HTTP/routing/relay now carries exact owner authority through VOD status,
  durable local/remote/terminal classification, status rerouting, release,
  remote abort, resurrection, range/`If-Range`, and EOF. Request preparation
  retains one inherited deadline; an admitted relay body owns a separate
  bounded streaming lifecycle. DELETE elects one cancellation-safe settlement
  token, duplicates join its exact result, commit-unknown End retries through a
  sharded bounded registry, and terminal authority is published before
  detached process reclamation.
- Rolling authority/settlement now makes Terminal admission irreversible once
  queued, finds the exact Session across durable-ID adoption, releases the
  global registry before actor/process settlement, publishes failure before
  kill/reap, and retains process admission plus scratch until confirmed reap.
  Detached retirement and scratch owners survive caller cancellation.
- VOD uses per-rendition single-flight ownership for slow builds, bounded and
  confirmed-reap missing-init regeneration, cancellation-safe dormant purge,
  immediate heavyweight terminal-graph compaction, and bounded compact `410`
  replay followed by fail-closed durable eviction.

The pre-rebase implementation frozen through rebased commit `611d60cf` joins
those tracks at their
shared publication boundary:

- replacement is make-before-break: the provisional successor activates by
  Store CAS before the authoritative predecessor is projected terminal, while
  persisted `publication_ready_at_ms` is an explicit three-state fence. Only
  `0` is publishable; activation writes an indefinitely blocked sentinel, the
  first exact post-commit observer arms a fresh 372-second not-before value,
  and exact acknowledgement or an exact elapsed-boundary Store proof clears
  it. Routing and idempotent replay never infer readiness from wall time;
- Store rows now retain the first `terminal_reason` across public release,
  supersession, admin/revoke paths, stale-owner cleanup, and node removal.
  SQLite and Hiqlite migrations, route queries, replicated dump/import, and
  old-schema import defaults carry both new fields;
- release is explicitly two phase: install a cause-neutral local publication
  fence, await the uncancelled first-writer Store End, cache its exact terminal
  route or definitive absence, then project that durable cause to the exact
  owner. A shared `204` is published only after replay-visible proof and owner
  acknowledgement, or after the finite admitted-body safety boundary. Ended
  rows reuse `publication_ready_at_ms` as a durable projection ledger:
  sentinel means the post-End boundary is not armed, finite means the one
  restart-stable fallback is pending, and `0` means exact acknowledgement or
  exact boundary proof was persisted. A restart never mints a second window;
- attempt-derived bodyless status and typed failure validate the exact
  Session/attachment and rolling attempt without renewing demand or advancing
  delivery. Streamed body authorization commits only after all advertised
  bytes have been read and accepted by the bounded HTTP-body channel; a fully
  buffered body commits only after exact preparation immediately before
  exposure and is then wrapped by the same absolute body deadline.
  Cancellation, short read, storage error, and abandonment before the final
  streamed chunk is accepted are non-commits; and
- every admitted local or relayed media body has a 300-second absolute
  lifetime and a 30-second upstream or downstream no-progress bound. Driven
  local bodies retain at most one unacknowledged chunk and account/commit only
  after the public Body accepts that chunk; the relay pump retains two and all
  public bodies discard queued bytes after a terminal or absolute deadline.

The unchanged pre-rebase content was locally validated. Rebased runtime head
`b1eeba78`, assertion/scanner head `2afa1bba`, compile/Clippy heads through
`77e292d1`, and historical-schema head `4d38a084` retain those earlier
adversarial approvals; the current serving-authority source repair is under a
fresh exact-head review.
The one allowed full local unit invocation has been consumed; every failed or
directly affected name now passes, and no broad unit target will run again.
PR #626's runtime commits
`c04898e2`, `732d3442`, `91486148`, `ba3a504d`, and `4d0a0c0f` add a
behavior-neutral, one-slot immutable actor decision, a non-consuming poll
command, and one bounded passive executor inbox/task. Root static review
repaired a mailbox self-retention cycle before the first commit. Adversarial
rounds 1 through 3 found timer-only terminal wake, executor-loss visibility,
terminal-cause projection, compile/lint, test-scheduling, actor-loss
classification, terminal/receiver-loss races, stale executor-state overwrite,
and a four-variant poll-contract violation. The final defensive pass also made
the first terminal-or-lost settlement win in either order. The runtime head
received unanimous exact-head approval at `981ebb51`. The focused Rust suite
then passed 85/85; the ownership inventory exposed six stale normalized
counts/anchors and one warnings-as-errors risk. `73cd33e9` fixed the normalized
counts and warning; the rerun cleared five failures and proved the remaining
entrypoint row was invalid because that table is deliberately source-local to
`transcode.rs`. `9d100a04` removes the misplaced row while whole-module
sentinels continue to cover the transport. Targeted review approved that
repair and the inventory passed 7/7. `make check` then reached the history gate
and stopped because four corrective mapping commits were not themselves named
by their existing evidence records. After that reviewed repair, the rerun
passed catalog, history, 121 validation tests, and 52 additional tests, then
Clippy identified the intentionally retained strong transport owner as unread
outside tests. `f463d4d4` documents and allows that one lifetime-owner field.
The repair received targeted approval, the corrected handoff received final
static approval, and the final documentation corrections received exact-head
approval at `1ead3944`. `make check` and `make cluster-check` passed. Hosted
run `33127652859` then passed every selected job and the aggregate PR
validation gate on that reviewed implementation/status head. Final exact head
`62a4f535` passed hosted run `33129200705`; PR #626 merged as `9063bb1e`.
PR #624 merged as `dba35f98` after unanimous exact-head review, green local
`make check` and `make cluster-check`, and hosted run `33118301414`. Its bounded
shared command/producer sequencing, publication-time ordering, stale-exit
fencing, observation-only actor projection, and command/deadline metrics are
now the validated baseline. Legacy watchdogs and recovery actions remain
active. The merged decision transport emits no production decision and owns no
retry, cleanup, response-admission, or process action. The active cut is the
first one authorized to take those duties from the compatibility path.
The detailed M4 contract is in
[`PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md`](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).
The resumable execution state is in
[`PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md`](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md).
**Source of truth:** this page tracks delivery; the design and acceptance
contracts remain in
[`PLAYBACK-CONTROL-PROTOCOL-PLAN.md`](PLAYBACK-CONTROL-PROTOCOL-PLAN.md).

This is the canonical done-versus-left ledger for the playback-control
rewrite. Update it in every implementation PR. A feature listed as partial is
not being counted as complete merely because its foundation has landed.

## At a glance

| Workstream | State | Shipped result | Still required |
|---|---|---|---|
| Design and adversarial review | **Complete** | End-to-end protocol, actor, replacement, cluster, index, observability, and watchdog-deletion contracts | Re-review each implementation PR against the contract |
| M1 — typed control plane | **Complete** | Strict v1 messages, capability route, sequence and owner fencing, bounded cluster relay, default-off advertisement, metrics | Active actions remain deliberately disabled |
| M2 — passive clients | **Partial** | Web reports demand, playhead, contiguous runway, render state, selections, capabilities, and recovery evidence | Apple and Android reporters; alternate-ingress physical evidence |
| M3 — actor and explicit lease | **Complete** | Rolling generation has one bounded actor for control fencing, explicit demand/pacing, renewal, expiry claim, typed End/authority-fence ownership, durable terminal replay, response commit ownership, the attempt-fenced delivery ledger, nonblocking progress/exact-exit observations, and exhaustive event-order evidence | M4 now moves recovery decisions through that owner |
| M4 — server watchdog removal | **In progress** | Merged contract, deadline-chain ingress, legacy-owner inventory, sequenced actor/producer ordering, and passive decision transport through PR #626; locally green PR #636 assembles production startup recovery, make-before-break publication fencing, explicit three-phase cluster activation, durable release/terminal truth, exact response settlement, routing/relay, VOD ownership, and exact cluster-takeover settlement | Exact-head review PR #636, satisfy hosted checks, and merge; later cuts remove published-lifetime and copy compatibility owners |
| M5 — one client action owner | **Not started** | — | Collapse web, Apple, and Android reopen/watchdog paths into one controller per platform |
| M5.5/M6 — prepared handoff and Auto | **Not started** | — | Staged generations and transactional resolution, bitrate, codec, HDR/Dolby Vision, audio, subtitle, and node changes |
| M7 — semantic indexes/subtitles | **Partial foundation** | Cluster-shared structural fragment index plus durable force-analysis queue and operator status page | Timeline annotations, exact intro/credits markers, feature sidecar, subtitle windows, seek coalescing, marker prewarm |
| M8 — cluster handoff | **Not started** | Control relay and owner fencing exist from M1 | Planned drain, VOD resurrection, rolling successor failover, compatibility-takeover retirement |
| M9 — cutover/deletion | **Not started** | — | Mixed-fleet evidence, defaults on, compatibility engine and `/status` polling deleted, physical matrix green |

## Merged evidence

| PR | Merged result | Verification at merge |
|---|---|---|
| [#600](https://github.com/pjunod/plurx/pull/600) | Detailed explicit playback-control design and independent adversarial finding ledger | Documentation review complete |
| [#602](https://github.com/pjunod/plurx/pull/602) | M1 fenced, behavior-neutral control plane and mutating cluster relay | Adversarial review, local gates, and hosted CI green |
| [#603](https://github.com/pjunod/plurx/pull/603) | Content-addressed VOD fragment-index work shared across cluster voters | Adversarial review, local gates, and hosted CI green |
| [#605](https://github.com/pjunod/plurx/pull/605) | M2 passive web control reporter and joined recovery evidence | Adversarial review, local gates, and hosted CI green |
| [#606](https://github.com/pjunod/plurx/pull/606) | Durable force-analysis requests, bounded queue/status API, Activity status UI | Merged with three red hosted jobs subsequently repaired by #610 |
| [#610](https://github.com/pjunod/plurx/pull/610) | Fixed all three #606 merge-gate failures and added the missing ops contract test | Adversarial review, local gates, and hosted CI green |
| [#611](https://github.com/pjunod/plurx/pull/611) | M3a bounded rolling lease actor and atomic expiry/retirement ownership | Adversarial approval at `dddc8d24`; full local gate and all hosted jobs green |
| [#613](https://github.com/pjunod/plurx/pull/613) | Preserved an empty Dolby Vision `hvcC` record when copying parameter sets upstream | Adversarial review, local gates, and hosted CI green |
| [#614](https://github.com/pjunod/plurx/pull/614) | M3b explicit demand lease, response-commit ownership, demand-based pacing, and operator instrumentation | Exact-head adversarial approval, full local gate, and all required hosted jobs green |
| [#615](https://github.com/pjunod/plurx/pull/615) | M3c1 actor-owned, exact-attempt publication/fetch ledger and fenced producer installation | Exact-head adversarial approval at `7dffa5f3`; full local gate and every required hosted job green; merged as `46c08439` |
| [#616](https://github.com/pjunod/plurx/pull/616) | M3c2 constant-space producer progress/exit ingress, exact-attempt actor facts, and one cancel-safe process supervisor | Exact-head adversarial approval at `4b818d39`; 12 focused tests, 1,927 full-workspace tests, every local gate, the long cluster gate, and all required hosted jobs green; merged as `8c6ccdf7` |
| [#617](https://github.com/pjunod/plurx/pull/617) | M3c3 typed actor-owned End, authority fence, and lease expiry; atomic durable terminal acknowledgement and exact replay; cancellation-safe settlement and exhaustive event ordering | Exact-head adversarial approval at `6f127463`; `make check`, `make cluster-check`, and every hosted job green; merged as `9cd16d05` |
| [#618](https://github.com/pjunod/plurx/pull/618) | M4 watchdog-removal ownership, deadline ordering, executor, cleanup, process-capacity, and post-publication proposal contract | Seven exact-head adversarial passes resolved 31 findings; final approval at `96799e60`; `make check`, `make cluster-check`, and hosted PR gate green; merged as `f9cef83b` |
| [#619](https://github.com/pjunod/plurx/pull/619) | Constant-space cutoff-safe producer progress coverage plus checked whole-module legacy owner/task/timer/process inventory | Thirteen exact-head reviews; final approval at `11315987`; focused tests, `make check`, `make cluster-check`, and hosted PR gate green; merged as `48ea494c` |
| [#621](https://github.com/pjunod/plurx/pull/621) | Passive producer deadline, cutoff-safe ingress/fencing, and process-flow barriers | Ten exact-head rounds ended unanimously **APPROVED**; hosted run `33109297301` green; merged as `8e331672` |
| [#624](https://github.com/pjunod/plurx/pull/624) | Bounded shared actor-command/producer sequencing and observation-only operational projection | Formal runtime round 5 unanimously approved; `make check`, `make cluster-check`, and hosted run `33118301414` green; merged as `dba35f98` |
| [#626](https://github.com/pjunod/plurx/pull/626) | Immutable actor decision slot, non-consuming poll contract, and bounded passive executor transport | Exact-head adversarial approval; focused Rust 85/85, ownership 7/7, `make check`, `make cluster-check`, and hosted run `33129200705` green; merged as `9063bb1e` |

## Active slice: M4 watchdog removal

PR #618 merged the complete M4 implementation contract at `f9cef83b`, after
seven adversarial passes and green local, cluster, and hosted gates. The actor
already owns explicit End, authority fence, and exact lease expiry; this slice
starts moving producer recovery evidence through that same owner.

PR #621 merged the passive deadline and cutoff-safe ingress foundation as
`8e331672`. PR #624 then merged bounded shared command/producer sequencing,
publication-time ordering, stale-exit fencing, partial observation-only actor
projection in `SessionInfo` and telemetry, and bounded command/deadline metrics
as `dba35f98`. PR #626 then merged the immutable typed decision contract,
non-consuming `PollProducerDecision`, one-slot actor pending value, and one
move-only executor inbox/task as `9063bb1e`. That merged transport remains
observation-only. The active branch binds it to production transcode startup:
the actor emits the first real decision, HTTP admission closes retry before
attempt bytes escape, and one executor applies the sole validated
prepublication retry.

M4 makes the same actor the sole recovery decision owner. It replaces hardware
startup grace, software startup/lifetime polling, and copy-segmenter fallback
with one exact-attempt `ProducerProgressDeadline`. One pre-publication retry is
allowed only when the actor supplies the validated recipe. After publication,
failure retains published bytes and produces one stable internal replacement
proposal; it never swaps the child in place or emits an incomplete wire action.
The existing exact-attempt supervisor remains the sole OS-child/PID owner, and
a single session executor orchestrates actor-authorized proxy changes. The full
contract is
[`PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md`](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).

Review and test state for M4:

| Gate | State |
|---|---|
| Implementation contract | **Merged in #618.** It defines deadline policy, contiguous cutoff-safe ingress with command and producer-event barriers, arm/disarm and event ordering, exhaustive action-timeout settlement, the one-retry invariant, hard rolling-process admission, process-executor ownership, publication-aware cleanup, post-publication proposal behavior, instrumentation, source ownership checks, and the race/failure matrix. |
| Static owner inventory | **Reconciled through `c4a1d96d`.** `tests/playback/rolling-producer-owners.toml` exact-counts recovery/election/replacement owners and scans every Rust module under `plurxd/src`. It names the candidate's retirement, exact shared release settlement, sharded commit-unknown reconciliation, takeover creation/worker/lease owners, durable handoff/terminal proofs, adoption/terminal gates, release/abort capacity, relay/prepared/local body owners, per-key VOD build, head-reap, purge, compaction, and tombstone owners. Hosted run `33226975369` exposed sixteen stale exact counts after the activation and cleanup repairs. Three reviewers independently recomputed the raw and normalized scans and approved the inventory-only repair; both exact failed methods pass 2/2 and the complete validation catalog passes 68/68. |
| Progress ingress | **Merged in #619.** `ProgressCoverageBatch` retains first, covered tail/deadline, first gap, latest progress, and latest telemetry with a persistent exact-attempt watermark and exit barrier. The local actor consumes that proof without flattening away publication time. |
| Passive deadline/cutoff | **Merged through #624.** The actor folds producer facts under transition plus ingress, uses fenced publication timestamps, classifies non-success exits immediately, preserves progress around bounded sequenced physical-flow barriers, fails closed when the actor/mailbox disappears, serializes actor-task exit fencing with producer transitions, and re-authorizes exact attempts before full-capacity STOP/CONT syscalls. It still emits no recovery action. |
| Operational projection | **Runtime repairs through `4da3bbde`; evidence closed through `44947095`; inventory reconciliation `c4a1d96d`; documentation tip is the review candidate.** Actor startup policy, contract fingerprint, response/retry cutoff, executor state, immutable decision application, copy/cache publication admission, first-media lifetime ownership, make-before-break replacement, transaction-safe takeover activation replay, paged takeover inventory, renewal-before-adoption publication, move-owned identity handoff, durable terminal/publication fields, two-phase release, exact status/error/EOF fences, bounded local/relay bodies, authoritative routing, and VOD build/cleanup ownership are assembled. Remote cluster activation now uses explicit negotiated ownership plus target serving generation, three-phase Prepare/Confirm/Publish-or-Abandon settlement with atomic claim deadlines on both Store backends, and restart admission owned through detached START completion. |
| Adversarial implementation review | **Every source and final-Rust repair is approved; immutable docs-bearing exact-head review is next.** The final rounds repaired response-publication cleanup, finite handoff renewal/takeover, current-owner replay after takeover, resolved-replay freshness, backend transaction parity, the v17→v18 Hiqlite trigger upgrade, remote START restart-drain ownership, executable trigger syntax, current fixtures, Clippy ownership shape, five Hiqlite placeholder-order defects, final transaction inventory/replay assertions, deterministic admission/terminal fixtures, and actor-bound playlist race seams. Exact head `032292a1…` was rejected for detached START ownership; the first cluster gate rejected six SQL bindings; hosted run `33228540108` exposed the last six exact Rust cases. All reviewed repairs now have unanimous approval with no remaining P0–P3 finding. |
| Format/static inspection | **Green on the repaired tip.** `cargo fmt --all`, `git diff --check`, compiler checks, workspace Clippy, validation catalog, history, 123 operations contracts, 52 benchmark checks, and the web policy/contrast gate pass. The hosted-equivalent Python validation catalog also passes 68/68 after `c4a1d96d`; exact history policy passes after docs-only mapping `9471d129`. |
| Unit/focused tests | **Green without a second broad local run.** The one full `make unit` invocation stopped during compilation and remains the only broad local invocation. Historical 641-test accounting is green. On the current repair, the exact activation Store contract, five daemon activation/replay tests, two SQLite migrations, two Hiqlite schema contracts, the placeholder-order invariant, and all six exact names exposed by hosted fast Rust pass. |
| Full/cluster/hosted gates | **All required local cluster evidence is green; hosted final-Rust repairs are exact-green and require one fresh hosted run.** Vendor WAL/Hiqlite probes passed. The replicated Store lane passed 61/67 before six exact placeholder failures; after `4da3bbde`, all six failed three-voter names pass. The complete cluster harness, seven activation tests, and two Activity tests pass. Hosted runs `33226975369` and `33227941555` exposed and closed inventory/history preflight findings. Run `33228540108` passed every other selected job, including the 23-minute replicated Store/topology gate, but fast Rust found four failures plus two obsolete-barrier hangs before its 30-minute cancellation. Those six exact names now pass after unanimous repair review. Merge remains conditional on immutable-tip approval and a wholly green hosted rerun. |

## Watchdog-removal ledger

The target is not zero timers. The target is no overlapping recovery owners.
The detailed symbol-by-symbol contract is in the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

Merged `main` has not reached that target: every legacy watchdog, replacement
path, action owner, and response-admission compatibility path remains active.
The active branch is replacing the prepublication transcode-start owner first.
Published-lifetime and copy recovery remain explicit later cuts and must not
overlap the actor-owned scope after their respective cutovers.

### Active-candidate watchdog disposition

| Mechanism | Active-candidate disposition | Why it remains, if retained |
|---|---|---|
| `FIRST_SEGMENT_GRACE` and the detached startup downgrade loop | **Removed from production.** `FIRST_SEGMENT_GRACE` has zero source owners, and `downgrade_one_step` is no longer a production recovery path. | The actor's one exact `ProducerProgressDeadline` and immutable retry decision replace them. |
| `SOFTWARE_GRACE` | **Retained outside actor-owned prepublication.** The first-media transfer starts the compatibility lifetime watcher without an additional startup grace. | Copy startup and published-lifetime compatibility still use the value until those bounded cuts move to the actor. |
| `PROGRESS_STALL` | **Retained in disjoint scopes.** The actor uses the explicit prepublication progress budget; compatibility code still uses the published-lifetime/copy threshold. | A producer cannot self-report a silent wedge. The later published/copy cuts must move the remaining decisions without overlapping prepublication. |
| `WATCHDOG_POLL`, `watch_for_stall*`, and `watchdog_active` | **Retained only for published-lifetime and copy compatibility.** | They cannot decide actor-owned prepublication recovery after this cut; later M4 cuts replace and delete those decision owners. |
| `child_transition` and `replacing_child` | **Retained for process/resource serialization and compatibility replacement.** | Exact child ownership, confirmed reap, published-lifetime handling, and copy replacement still require serialization until the later cuts remove in-place replacement. |
| copy fallback/recovery | **Recovery retained; response authorization actor-fenced in the candidate.** | The candidate orders compatibility failure before kill/reap and admits copy/cache responses through the actor, but copy recovery decisions have not moved into it. |
| `PREPUBLICATION_REAP_RETRY` (5 s) and `PREPUBLICATION_REAP_ATTEMPT_TIMEOUT` (2 s) | **Retained lifecycle cleanup bounds, not playback watchdogs.** | They retry bounded termination until the exact predecessor is confirmed reaped while retaining process admission and scratch; they cannot choose retry, replacement, playback failure, or response publication. |
| rolling retirement/scratch settlement | **New bounded lifecycle owners.** | Exact retirement survives caller cancellation and holds process resources through reap. Scratch cleanup makes three 5-second attempts separated by 5 seconds, then leaves the orphan for startup/maintenance cleanup; it makes no recovery decision. |
| exact-EOF settlement task | **Retained bounded response completion owner, not a watchdog.** | At most 256 visible streams can reserve one owner; each owner gets one fresh five-second deadline at exact advertised EOF, commits only complete delivery, and releases capacity on every terminal path. |
| first-media settlement | **Retained bounded handoff owner, not a watchdog.** | At most 256 settlements bridge actor authorization to published-lifetime ownership without clearing confirmed-reap state early. |
| absolute playlist preparation budget | **Retained network lifecycle limit, not a watchdog.** | One deadline covers registry/storage reads, actor observation, catalog projection, and every poll across reclassification; timeout returns retryable `503` and cannot select or replace a producer. |
| bounded HTTP actor/handoff waits | **Retained network lifecycle limits, not recovery watchdogs.** | One five-second absolute publication deadline covers admission, first-media application, buffered commit, and actor command/reply waits; cancellation is fail-closed before later actor mutation. |
| admitted local and relay body lifecycle | **New post-admission network/storage owner.** | Preparation uses the inherited request deadline; after admission, every prepared, local-file, and relay body has a 300-second absolute lifetime, while active file/network pumps also enforce 30 seconds without upstream or downstream progress. Independent bounded pumps own file/network readers and EOF authority under socket backpressure: local bodies retain one unacknowledged chunk, relay bodies two, and queued data is rejected after terminal/absolute timeout. The common finite resource lifetime makes the terminal-projection safety bound meaningful; M8 may change transport, but body-liveness bounds remain. |
| make-before-break successor publication | **New durable handoff fence, not a playback watchdog.** | A provisional successor must exist before the Store pointer CAS can end its predecessor. Activation persists an indefinitely blocked sentinel; the first exact post-commit observation arms a 372-second boundary, and publication requires an explicit Store transition to `0`. Exact predecessor acknowledgement may clear early because it prevents future old-capability admission; already-admitted bodies may drain under the predecessor's distinct never-reused URL. Without acknowledgement, only an exact elapsed-boundary proof clears the fence. M6 generalizes this transaction to quality/HDR/track changes. |
| public release and internal abort settlement | **New bounded terminal owners.** | Public DELETE elects one exact shared settlement before immediately detaching ownership; duplicates join without another task or permit. Five seconds bounds capacity admission and each caller's wait, never an admitted Store mutation. A commit-unknown End remains fail-closed in a 128-entry, 32-shard registry and retries at fanout four with 1–30-second backoff under panic supervision. The first Store `terminal_reason` wins. Ended rows persist sentinel → finite boundary → `0`, so exact owner acknowledgement and fallback completion survive ingress/owner restart without extending the window. Replay-visible terminal/absence proof plus durable projection completion settles `204`; an unreachable peer may settle only after the 372-second terminal-projection boundary covering the 62-second pre-header ceiling, 300-second body lifetime, and margin. Remote delivery is capped at three attempts with 100 ms delay, while physical reclamation remains independently bounded. M8 prepared handoff may replace the cleanup transport. |
| VOD missing-init regeneration | **New bounded lifecycle fallback.** | Four node-wide slots, a 15-second deadline, an 8 MiB ceiling, and per-key ownership through confirmed child reap prevent unbounded forks/buffers and same-key overlap. Later VOD/index hardening may delete on-demand regeneration. |
| VOD terminal cleanup/replay | **New compact lifecycle owner.** | Heavy media/process graphs release immediately after exact cleanup. A compact owner replays `410` for 60 seconds, then a 3-second, 16-way fail-closed durable confirmation permits eviction. M8 may centralize the durable replay record. |
| VOD idle/dormant retention | **Retained cache lifecycle policy.** | Sessions idle after 300 seconds; renditions become dormant after 1,800 seconds. Detached purge owns the key, child, accounting, directory, and identity cleanup before any await, preventing rebuild overlap. |
| cluster lease/takeover timers | **Retained failure fencing, not playback-progress watchdogs.** | Three-second renewal and 12-second lease bounds distinguish a live owner from a dead node. M8 replaces compatibility takeover with prepared handoff; lease expiry remains. |
| 15-second repair/flow-control tick | **Retained maintenance schedule.** | It refreshes indexes, retention, speed, pacing, and lease projection. M4 removes recovery authority from the tick; periodic maintenance remains. |

### Server mechanisms still present on merged `main`

| Mechanism | What it currently does | Removal owner |
|---|---|---|
| first-segment/software grace and progress watcher | Polls startup/progress and can replace or kill the encoder; it no longer needs to be the source of process-exit truth after M3c2 | M4 replaces this watcher with one actor-owned `ProducerProgressDeadline`; the exact-attempt process supervisor remains as event delivery, not a watchdog |
| `child_transition`, `watchdog_active`, `replacing_child` | Serializes and masks the old in-place replacement paths | M4 deletes them after every child event/action enters the actor |
| playlist and live-segment wait budgets | Bound an HTTP request waiting for publication | M3c1 records publication in actor state; later M3 routes waiters through it; bounded HTTP deadlines remain by design |
| 15-second repair/flow-control tick | Refreshes indexes, prunes retention, records speed, evaluates flow, and claims lease expiry | Scheduling remains; M3c1 copies publication/fetch facts into the actor but recovery decisions still move in M4 |
| `SegmentIndex`, `playlist_published`, `high_segment`, `fetched_end_ms` | Catalogs rolling files and feeds pruning, byte accounting, pacing, and status | M3c1 makes actor delivery state authoritative for status while retaining these as action-path compatibility projections until M4 |
| fetched-frontier ahead-window inference | Suspends/resumes production based on download behavior | M3b replaced the time policy only after a session enters explicit mode; legacy pacing and byte/disk safety remain |
| VOD segment materialization deadline | Bounds demand for an immutable segment that is not ready | Remains permanently as one of the three approved progress deadlines |

### Client recovery owners still present

| Client | Remaining independent recovery paths | Removal owner |
|---|---|---|
| Web | persistent-wait/startup/seek timers, hls.js fatal handlers, decode rescue, Auto rung replacement, truncated-end reopen, `/status` polling | M5 makes the controller the only action owner; M6 moves Auto to prepared actions; M9 deletes old polling |
| Apple | status polling, starvation/stall/black-frame monitors, reopen budgets/queue, early-end and item-failure handlers | Apple M2 reporter, then M5 controller cutover |
| Android | status polling, buffering stall tracker, stall guards/budgets, direct reopen coordinator, error/end compatibility handlers | Android M2 reporter, then M5 controller cutover |

After cutover, only these playback progress deadlines may remain:

1. server `ProducerProgressDeadline` for a producer that cannot report its own
   wedge or death;
2. client `PlaybackProgressDeadline` because only the playback framework knows
   whether decoded media is rendering; and
3. VOD `SegmentMaterializationDeadline` because an HTTP request cannot wait
   forever for a demanded immutable object.

Lease expiry, cluster owner expiry, preparation expiry, retirement grace,
HTTP/relay limits, admission waits, and control rate windows also remain, but
they are named lifecycle/network bounds rather than competing playback
recovery watchdogs.

The 60-second terminal-ack retention window introduced by M3c3 is likewise an
idempotency bound: it retains one immutable accepted reply and triggers no
playback, replacement, restart, or failure decision. It is not a watchdog.

Cluster takeover adds one 24-second, one-shot ownership lease. The survivor
must complete an exact bootstrap renewal from the provisional worker before
durable-ID adoption and seed/publication; ordinary renewals then return to the
12-second cluster owner lease. This fixed lease and its four-second Store
deadline can fence serving authority, but they never infer a playback stall,
select quality, or restart a producer. They are cluster split-brain bounds,
not playback watchdogs.

## Remaining delivery order

1. Freeze and review the exact repair tip, push it, make the hosted checks
   wholly green, merge it, migrate
   published-lifetime and copy recovery, and prove the old server watchdog/
   replacement symbols are gone.
2. Complete Apple and Android M2 reporters and record timer/alternate-ingress
   behavior.
3. Complete M5/M5.5/M6 so quality, codec, dynamic range, tracks, subtitles, and
   node placement use the same prepare/commit/abort transaction.
4. Complete semantic indexing: exact intro/credits destinations with
   provenance/confidence, manual overrides, subtitle readiness, queue metrics,
   and marker-destination prewarm.
5. Complete clustered planned/hard handoff, mixed-fleet cutover, compatibility
   deletion, playback-lab fault injection, and physical web/Apple/Android
   acceptance.

## Definition of finished

This project is finished only when M9 acceptance passes: repository checks
find only the three approved progress deadlines and named lifecycle timers;
no recovery path outside the actor/controller can replace media; transparent
prepared handoffs cover the supported recipe axes and cluster ownership;
semantic analysis publishes exact skip destinations; and the automated plus
physical playback matrices are green. Merged foundations are not a substitute
for that exit condition.
