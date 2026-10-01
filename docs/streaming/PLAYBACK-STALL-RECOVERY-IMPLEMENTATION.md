# Playback stall repair — recover correctly and explain the next quorum failure

**Status:** implementation candidate complete; one adversarial review
addressed; final validation pending; initiating cause and fencing-policy ruling
remain open; no deployment claimed ·
**Written:** 2026-10-01 EDT · **Incident build:** `91917940e` ·
**Reference base:** Forgejo `main` `91917940eb0b1f1b3eefd0758efb2054019ac3fe`.

Companion to [PLAYBACK.md](../PLAYBACK.md) (delivery behavior), the
[playback surface contract](../clients/PLAYBACK-SURFACE-CONTRACT.md)
(which owner may stop playback), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (delivery workflow).
This document owns the bounded repair of the October 1 Safari incident on
nynuc. Read the current [AGENTS.md](../../AGENTS.md) and
[web source map](../clients/WEB-SHELL-LAYOUT.md) before building. Re-resolve
the symbols below against the implementation base; concurrent playback work
must be integrated, not overwritten.

## 1. Delivery boundary — one repair PR, three local checkpoints

Build one coherent corrective change on `codex/playback-stall-recovery`, in
an isolated checkout based on current remote `main`. Keep normal commits
and one draft main-bound PR. The checkpoints below are implementation and
verification boundaries, not separate PRs, separate review campaigns, or
new projects. This is a bounded incident repair using the ordinary
main-bound lane. If implementation actually grows into a multi-task effort,
apply the repository's effort-branch rules explicitly before splitting it.

Keep delivery-label/Activity corrections in separate commits inside this
PR, with their own regressions and review section. They overlap the VOD
control/descriptor and web policy/test files used by the incident repair;
this is not a disjoint-file exception to the effort rules. The Opus
suggestion to split a second PR is not adopted: one bounded review and
fast-lane run better fit the requested delivery pace. Do not add rich
Activity drill-down APIs or native-client UI work to this scope.

1. Add causal diagnostics and reproduce the failures.
2. Repair server lifetime/cleanup and web recovery together.
3. Run focused acceptance, obtain one adversarial implementation review,
   address it, run the required fast lane, and deliver the repair.

The missing quorum cause must not hold the proven repairs hostage. If one
bounded diagnostic pass identifies a narrow fix, include it before final
review. If the trigger requires a deployed diagnostic build, land the
confirmed repairs and diagnostics, then make one evidence-driven follow-up
for that specific cause. Report **repair delivered; initiating quorum cause
open** until it is proved. Never call a quiet observation window a root cause.

**No feature gates.** Startup, retirement, cleanup, recovery and causal
logging corrections are always active. Add no rollout flag, hidden config,
environment opt-in, per-node allowlist, readiness prerequisite or new setting
for these fixes. No Developer card is needed. If a genuinely optional,
expensive diagnostic mode proves indispensable, its maximum control surface
is an explicit Settings → Developer switch with requirements shown as Met /
Not met, advisory only. Readiness must never disable the switch, reject Save
or override the saved choice. Follow the
[Developer lifecycle](../features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md#developer-lifecycle--every-card-graduates).

**Non-goals:** a player rewrite, a new telemetry service, an all-client UI
rewrite, Raft replacement, speculative connection-pool work, a general runner
optimization program, new CI lanes, or repeated full test campaigns. Preserve
quorum fencing and identity checks in this repair. The invariant is that a
stale authority generation cannot continue mutable production, claim a
successor's ownership, or resurrect its retired actor after recovery.
Permanent retirement on every short proof gap is the current policy that
enforces that invariant; its blast radius remains an explicit design
question below. Proof-lease sizing is outside this PR. After attribution,
it may be reviewed against measured timing and the existing
`lease < election_timeout_min` constraint, without substituting for a fix.

### 1.1 Root-cause and architecture requirements apply to every repair

The user explicitly rejects a return to overlapping watchdogs and symptom
patches. Before implementing each behavior change, record this chain in the
PR: **observed failure → concrete cause → violated invariant → existing
owner → correction → regression that fails before the correction**. If a
link is unknown, instrument that boundary first; do not invent a remedy.
The bounded investigation limit caps effort, not the standard of proof.

| Failure | Violated invariant / owning component | Architectural correction |
|---|---|---|
| Healthy cluster cannot refresh proof in time | A valid proof must be obtained within its safety budget; cluster authority owner | Identify the delayed stage and initiating mechanism, then repair that mechanism or measured host resource allocation |
| A brief shared proof gap terminates rolling streams fleet-wide | Current loss-generation policy permanently revokes admitted mutable actors | Record this amplification separately from the trigger; preserve policy in this PR and resolve the explicit architecture ruling below |
| Early pause leads to startup expiry | Startup and hold are distinct actor-owned lifetimes | Correct the existing actor's evidence and deadline transitions |
| Play resumes an ended session | The current terminal presentation cannot be resumed as live | Preserve terminal identity/cause and use the existing reopen owner |
| A temporary authority refusal permanently stops control | A retryable authority refusal does not end the control session | Classify the response in the existing reporter's retry/backoff path |
| Separate outage inherits spent budget | Recovery allowance belongs to a failure episode | Correct the existing recovery owner's state lifetime |
| Keep waiting does nothing | An offered action must have an executable owner transition | Make action eligibility agree with the existing attempt state |
| Sweeper removes retained objects | Exactly one lifecycle owns directory deletion and scratch release | Restrict crash-leftover sweeping to startup before admission; current-process cleanup stays with retirement |
| A pause's origin is ambiguous | Observations do not identify the command that caused them | Preserve causal identity at existing transport entry points |
| Watch page says live fallback while session is VOD | Catalogue capability is not current delivery; active attachment owns delivery truth | Render session delivery from its authoritative descriptor; align capability reads with supported index stores |
| Activity omits VOD diagnostics and defaults to Active | Delivery inventory must preserve observed client/producer facts without inventing health | Extend the cheap bounded VOD inventory with existing observations; leave manifest-backed detail in Playback Info |

No new periodic recovery loop, independent timer owner, fallback controller,
title-specific condition, node-specific playback exception or duplicate
session registry is permitted. Inventory the touched startup, stall,
control, retry and cleanup timers before editing. For each, name its owner,
purpose, cancellation and successor rules; remove redundant branches made
obsolete by the correction. Use one existing owner for each transition.
The healthy-episode interval in §6 is evaluated by the existing presentation
sampler, not a new watchdog or a new reason to reopen media.

Reviews must reject a patch that only reduces the visible frequency of an
unexplained failure. Diagnostics may ship while a cause is open; recovery
can be corrected because its own state-lifetime defect is established. But
neither is evidence that the initiating cluster defect has been repaired.
Preserve the separate completion criteria in §9 and do not close the
incident until the initiating cause is concretely demonstrated.

**Open architecture ruling — fencing blast radius.** All proofs use the
leader, so one shared delay can cause every node to retire mutable sessions,
even when proof returns quickly. The measured absence of proof on all
voters does not establish that takeover was impossible: observations can be
asymmetric and arrive at different times. Before changing permanent
retirement, require a concrete authority/takeover timeline covering a true
minority partition, delayed old commands, successor admission and recovery.
The decision for Paul is whether to retain permanent teardown or commission
that narrowly scoped suspend/resume design after attribution. Recommendation
for this repair: retain the existing policy. This decision does not block
diagnostics or proven lifecycle fixes; no policy change is implied by them.

## 2. Incident baseline — facts, reproduced mechanisms and remaining hypotheses

The viewer used Safari web playback of Heated Rivalry S1E1, file ID 645,
through nynuc. The deployed build selected rolling copy-HLS while the VOD
index was pending. The following times are **2026-10-01 EDT**; add four hours
for the UTC server logs.

| Time / observation | Established conclusion | Limit |
|---|---|---|
| 12:02:42: nuc4, m6 and nynuc lost serving authority within 82 ms | Expired quorum proof fenced mutable media; nynuc terminated the producer and Activity removed the session | No leader change, container restart or OOM was observed; the delayed proof's underlying cause was not retained |
| 12:03:12–17: automatic reopen and playback resumed | Recovery worked once | A new presentation still had to satisfy the startup actor |
| Viewer paused around 2:03 into the episode; replacement expired at 12:03:45 | `startup_expired` won despite prior visible playback and continuing control reports | Complete intermediate control snapshots were not persisted; user confirmed the pause |
| 12:03:49: orphan sweep removed retired directories | The sweeper did not recognize retired ownership | Later cleanup repeatedly reported missing files and kept promises charged |
| 12:13:54: all three voters lost proof again | A separate cluster interruption killed the current stream | A seek's ordinary ownership fence must not be mislabeled as global quorum loss |
| 12:14:22: exhausted overlay; screenshot at 12:16:26 shows 7:03 | Recovery count was still spent; the exhaustion owner internally paused playback | The paused icon is not evidence of a viewer Pause command |
| 12:30:05: all three voters lost proof; nynuc killed the stream; buffer ran out at 12:30:42 and exhaustion followed at 12:30:50 | A third captured interruption has the same authority-loss → producer termination → buffer exhaustion chain | The delay's initiating mechanism remains unknown |
| 12:32:36: another shared authority loss after a manual recovery | The cluster problem recurred independently of recovery-budget policy | That recovery selected VOD; do not assume this brief loss killed the new immutable presentation |
| Cluster index artifact built at 12:25:33.430; VOD attached at 12:32:30.600 and again at 12:33:00.590 | A subsequent explicit reopen could use the completed index | Index completion was about 4 minutes 32 seconds before the 12:30:05 fence, not an observed in-place switch at the stall |
| 12:33–12:34 screenshots: watch ledger says live fallback; Activity and Playback Info say VOD; Control includes `serving_fenced` | Session and catalogue-based labels disagree; VOD mode does not erase a control-authority failure | Producer `held` means ahead-window pacing, not a viewer pause |
| Wider log window, 10:39–12:50 EDT: 32 authority losses on nynuc | The four playback-correlated losses above are a subset of a recurring fleet incident | Several series repeat near 16 minutes; exact loop attribution still needs timing evidence |

Re-reading the retained nynuc log for **14:39–16:50 UTC** confirms 32
loss/recovery pairs and 16 pairs of loss timestamps separated by 960 ± 2
seconds. Examples are 16:02:42 → 16:18:42 → 16:34:42 and 16:00:35 →
16:16:35 → 16:32:36 → 16:48:36 UTC. Not every loss fits an exact period.
The Opus fleet review reports cross-voter coincidence within about 80 ms;
the original four playback windows were independently correlated across
voters. The full nynuc capture gives loss-to-recovery intervals of roughly
0.052–1.692 seconds, rather than the review's narrower duration range.
These intervals describe logged authority state, not request duration.

The investigation reproduced the early-pause startup mechanism, missing-dir
cleanup failure, recovery count retention and ineffective Keep waiting path
with isolated tests extracted from deployed source. Those probes explain
mechanisms; they are not repository regressions or full browser acceptance.
Port them into the existing test harnesses below.

**Leading unproved initiating cause:** periodic application work delaying
leader proof/apply progress. Library-channel reconciliation's 30-second
initial delay and 15-minute sleep after completion fit the repeating
pattern if a run takes about one minute; that runtime is a hypothesis, not
a measurement. Other candidates remain host/runner contention, network delay,
runtime scheduling delay, leader quorum RPC delay, and state-machine apply
wait. Fresh WebSocket creation for authority reads is a candidate contributor,
not a measured cause of either interruption. The proof loop uses a 500 ms
refresh, 750 ms request timeout and one-second lease anchored at request
start. This explains sensitivity to a delayed sample; it does not identify
what delayed it.

## 3. Source ownership and existing interfaces

One builder owns the integrated patch. Use these existing seams; introduce
no second recovery controller or directory-accounting authority.

| Surface | Existing source / symbols | Required work |
|---|---|---|
| Proof sampling | [migration.rs](../../crates/plurx-core/src/cluster/migration.rs), `status::run_quorum_watermark_loop`, `record_watermark_error`, `ServingProof` | Bounded failure/expiry evidence and proof timing |
| Leader proof | [mgmt.rs](../../vendor/hiqlite/src/client/mgmt.rs), `db_quorum_watermark_local` | Attribute leader-check timing without changing proof semantics |
| Transport | [raft_client.rs](../../vendor/hiqlite/src/network/raft_client.rs), `NetworkStreaming::new_client` | Instrument only the transport stages needed to distinguish the measured failure; no speculative pooling rewrite |
| Media authority | [serving_fence.rs](../../crates/plurxd/src/serving_fence.rs) | Join fence/recovery events to the last proof attempt and loss generation |
| Periodic application work | [library_channels.rs](../../crates/plurxd/src/http/library_channels.rs), `reconcile_loop` / `reconcile_once`; [state.rs](../../crates/plurxd/src/state.rs), `work_pretranscode_queue` | Correlate actual reconcile/per-channel and cachekeep work with leader proof/apply delay |
| Startup / hold | [playback_control.rs](../../crates/plurxd/src/playback_control.rs), `RollingStartupState::observe_control`, actor control/expiry | Pause-aware startup lifetime and exact terminal cause |
| Control response | [control.rs](../../crates/plurxd/src/http/hls/control.rs), [manager control](../../crates/plurxd/src/transcode/manager/control.rs) | Preserve terminal cause through the existing error response |
| Directory ownership | [maintenance.rs](../../crates/plurxd/src/transcode/manager/maintenance.rs), `sweep_orphan_dirs`; [state.rs](../../crates/plurxd/src/state.rs), `DueJob::CleanupTranscode`; [retirement.rs](../../crates/plurxd/src/transcode/rolling/retirement.rs), cleanup owner / `clear_session_dir` | Startup-only crash sweep before admission; retirement owns current-process cleanup; idempotent release |
| Web intent / recovery | [player.js](../../crates/plurxd/src/web/player/player.js), [decode-margin.js](../../crates/plurxd/src/web/player/decode-margin.js), [transport.js](../../crates/plurxd/src/web/player/transport.js), [directed-change.js](../../crates/plurxd/src/web/player/directed-change.js), [measurements.js](../../crates/plurxd/src/web/player/measurements.js), [stall-diagnosis.js](../../crates/plurxd/src/web/player/stall-diagnosis.js), [surface.js](../../crates/plurxd/src/web/player/surface.js) | Intent provenance, paused reopen, recovery episodes and working actions |
| Control exchange lifetime | [playback-control.js](../../crates/plurxd/src/web/playback-control.js), reporter `drain` error classification / retry scheduling | Retry temporary authority refusal; preserve authoritative terminal facts across later intent changes |
| Policy / initialization | [playback-policy.js](../../crates/plurxd/src/web/playback-policy.js), [decode-tiers.js](../../crates/plurxd/src/web/player/decode-tiers.js) | Pure recovery policy and per-player state |
| Persisted client evidence | [system.rs](../../crates/plurxd/src/http/system.rs), `client_log`; [telemetry.rs](../../crates/plurxd/src/telemetry.rs) | Validate and retain bounded causal fields using existing storage |
| Delivery / capability display | [watch-browser.js](../../crates/plurxd/src/web/detail/watch-browser.js), `watchDeliveryRow`; [browse.rs](../../crates/plurxd/src/http/browse.rs), file `vod_index_status`; [VOD creation](../../crates/plurxd/src/vod/serve/create.rs) and [cluster index lookup](../../crates/plurxd/src/vod/serve/construct.rs) | Distinguish attached delivery from file capability and reconcile legacy/cluster index knowledge |
| Activity VOD diagnostics | [rolling/session.rs](../../crates/plurxd/src/transcode/rolling/session.rs), `vod_delivery_session_info`; [delivery.rs](../../crates/plurxd/src/vod/serve/delivery.rs), `delivery_infos_bounded`; [vodserve.rs](../../crates/plurxd/src/vodserve.rs), `VodDeliveryInfo`; [internal_activity.rs](../../crates/plurxd/src/http/internal_activity.rs), `ActivityDelivery`; [activity-stream.js](../../crates/plurxd/src/web/pages/activity-stream.js); [helpers.js](../../crates/plurxd/src/web/detail/helpers.js), `fmtBytes` | Cheap observations and explicit unknowns through local/peer inventory; no manifest reads or selected-session endpoint |

Existing internal interfaces to preserve or extend in place:

```rust
// Existing methods; recheck their containing types on the build base.
async fn sweep_orphan_dirs(&self) -> usize;
async fn clear_session_dir(dir: &std::path::Path) -> std::io::Result<()>;

// PlaybackDemandSnapshot already carries these observations:
// demand, position_ms, render_state, seek_target_ms, request_fingerprint.
// Generation/owner identity remains on LocalControlRequest.
```

```javascript
// Existing browser entry points, all in the shared script scope.
pausePlaybackInternally(v);
togglePlay();
finishStallRecovery(outcome, note);
playbackSurfaceAction(action);
retryPlayback();
```

New names and fields in the following sections are proposed contracts, not
claims that the APIs already exist. Prefer existing files. A new web script
requires its asset registration, shell tag, source-map row and load-order
checks together, as the web source map specifies.

## 4. Checkpoint A — enough evidence to name the next failure

### 4.1 Quorum evidence must survive ordinary production logging

Emit a structured warning for the first failed refresh and every serving
authority loss. Emit a recovery event with outage duration and suppressed
repeat count. Coalesce repeated identical sample failures to at most one
summary per ten seconds per node; never suppress a new loss generation.
Record all failures in counters even when text is coalesced. Do not log every
successful 500 ms sample.

**Capture at expiry, while the next request is still in flight.** The
sampler is serial, uses a 500 ms interval with skipped missed ticks, and
anchors the one-second proof lease at request start. In a normally aligned
cycle, the preceding proof can expire about 500 ms into the next attempt;
its 750 ms request timeout can arrive about 250 ms after the fence. Exact
remaining tolerance depends on prior timing. A later failure record alone
does not explain the boundary that killed playback.

Keep a small process-local snapshot of the current attempt: sequence,
monotonic start, scheduled start, current phase/phase start, target leader,
and latest completed outcome. Update it at the existing boundaries. The
authority-loss record reads that snapshot immediately and includes attempt
age and phase at expiry, even if the request has not failed yet. Sampling
this evidence must not await that request or perform a store/leader call.
Publish a coherent snapshot; do not combine fields from two attempts. A
later completion/timeout joins by attempt sequence and cannot retroactively
renew an expired generation. This adds evidence to the current owners, not
another timer or authority source.

Use bounded records with these fields:

| Fields | Meaning / interpretation |
|---|---|
| `node`, `build`, `attempt_seq`, `loss_generation`, UTC timestamp | Join a node's sample, fence and media termination; sequence is local, not a global request identity |
| Monotonic request start, elapsed duration, scheduled-tick lateness | Separate a late-starting sampler from a slow request; monotonic values compare only within one process |
| Last successful proof age, remaining lease, refresh/timeout/lease constants | Explain exactly why authority expired; request completion must not renew the original timestamp |
| Outcome `timeout`, `source_error`, `proof_rejected`, `cancelled` | Preserve the actual class; normal shutdown cancellation is not a quorum failure |
| Last observable phase and phase duration | At least local scheduling, request/leader round trip, leader linearizable check and local publication; use `unknown` for an unavailable stage |
| Term, leader ID, committed and applied indices, local watch age | Distinguish leadership transition, stale observation and apply lag |
| Bounded error category and sanitized text | Preserve the reason without credentials, media paths or unbounded upstream strings |

Where the leader's linearizable call does not expose quorum versus apply
timing, report `linearizable_check` honestly. Add the smallest available
boundary instrumentation before claiming a finer distinction. Cross-node
correlation uses UTC plus clock-offset evidence and IDs where available;
never subtract two processes' monotonic clocks. Mixed-version peers must
remain usable: missing optional diagnostics mean unknown, not refusal.

Reuse existing local logs, telemetry and support-bundle paths. Ensure the
failure/expiry records survive the normal logging filter and are available
after routine WebSocket chatter has overwritten the UI ring. A regression
must exercise the actual filter/persistence route, not merely a formatted
string. Keep metric labels low-cardinality; attempt, session and command IDs
belong in event records, never metric labels. Do not add replicated writes
to every proof refresh.

### 4.2 Test periodic application work first, then resource contention

The 32-loss record and near-16-minute sequences make application scheduling
the first hypothesis to test. In `library_channels::reconcile_loop`, a
30-second initial wait precedes `reconcile_once`; the next periodic pass
waits 15 minutes **after completion**. Catalogue mutation notifications can
also schedule a pass after a 30-second debounce. Instrument both triggers.
A one-minute pass would explain the period, but no existing trace proves
its duration or identifies a blocking channel. The other candidate is the
15-minute clock-gated cachekeep work in `work_pretranscode_queue`. Timing
similarity alone cannot convict either loop or exclude a concurrent CI job.

**First bounded pass:** capture up to 35 minutes across all voters, enough
for two expected recurrence intervals, with one start/end record per
reconcile run, trigger, node, run sequence, channel duration and outcome;
record cachekeep start/end too. Keep identifiers in bounded event records,
not metric labels. Join these records to leader nuc4's linearizable/apply
timing and the in-flight proof snapshot in §4.1. Capture the actual queue,
lock or scheduling boundary if the long operation overlaps the proof gap.
Use one isolated targeted reproduction to distinguish mere overlap from a
mechanism, such as a specific store operation holding the contested resource.
Do not disable a production channel or force a heavy reconcile during the
viewer's session merely to create a failure.

If this identifies a concrete mechanism, fix its owning operation and
repeat the focused reproduction; do not proceed to a runner experiment by
rote. If no causal relationship emerges, retain the negative result and
test the runner hypothesis below. In either case, correlated host evidence
can distinguish a loop's direct blocking from workload-induced contention.

The user identified nynuc's dual role as media node and Forgejo runner.
An older [runner investigation](../ci/NYNUC-RUNNER-ORPHANED-SLEEP-PROCESSES.md)
records memory pressure counters and orphaned stopped processes on September
8. That is context, not evidence for October 1. Follow
[runner disk ownership](../ci/RUNNER-DISK.md) and the existing
[service resource work](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md)
instead of starting another cleanup project.

**Runner historical pass:** map jobs actually assigned to nynuc to the
retained loss windows, including job ID, runner, workload, start/end and configured
concurrency. Collect whatever host/cgroup samples exist for those windows;
record missing historical samples as missing. Ten-minute averages and
since-boot counters cannot establish a sub-second cause.

At 12:33:12 EDT, a read-only snapshot found five active nynuc runner
services, CPU pressure `avg10=0.00`, memory pressure `avg10=0.24`, and I/O
pressure `avg10=3.69`. These are Linux PSI percentages, sampled after the
failures; active runner services do not prove active jobs. Leader nuc4's
contemporaneous CPU/memory/I/O `avg10` values were zero. Retain these as
context only, not attribution or evidence excluding a short earlier spike.

**Conditional runner comparison:** only if the first pass does not establish
the initiating mechanism, collect up to 30 minutes of ordinary CI activity
and the same playback path, then up to 30 minutes with new job dispatch to
nynuc drained through the supported runner controls. Let active jobs finish;
do not cancel another user's build or kill runner processes. Do not generate
an unbounded stress workload on the production viewer's host. Keep the media
build, source and client path comparable. Capture at one-second intervals:

- CPU run queue, scheduler/cgroup throttling deltas and CPU pressure;
- memory pressure, reclaim, swap-in/out and cgroup memory-event deltas;
- disk latency/queue, I/O pressure and devices shared by Docker, Cargo and
  the transcode work directory;
- network retransmission/drop deltas and relevant peer latency;
- proof attempt duration, tick lateness, term/indices and fence events on
  nynuc **and** leader nuc4 / voter m6;
- actual runner job/container cgroup membership, including containers that
  are not charged to their runner service's cgroup.

**How to read it:** overlapping CI jobs alone prove overlap. A causal claim
needs a pressure spike that aligns with a named proof stage, a plausible
quorum-wide propagation path, and a controlled change or isolated replay
that removes that mechanism. Leader nuc4 can normally form a majority with
m6 without nynuc; explain why that path was affected before blaming nynuc
alone. A quiet drained run reduces suspicion but does not prove causality.

If contention is demonstrated, make the narrow operational correction at
the actual runner/job resource boundary: measured concurrency reduction,
CPU/I/O allocation, or moving the affected CI workload. Record queue-time
impact and playback/proof timing before and after. Do not hard-code CPU or
memory limits without observing capacity, weaken required checks, or rerun
every CI job for the experiment. A runner change needs its own reproducible
configuration and rollback; it is not a software feature switch.

Cap this investigation at the initial periodic-work window, the conditional
two runner windows and one targeted reproduction of the best-supported
mechanism. Preserve the result and one named next experiment if inconclusive.
Do not turn missing historical telemetry into a claim that CI was innocent
or responsible. No multi-day soak is a prerequisite to shipping confirmed
fixes; the initiating cause remains open until traced and reproduced.

### 4.3 Persist transport intent, not guesses from `paused`

Add a small causal record to the existing client-log event envelope:
`command_id`, action (`play` / `pause`), origin, reason, attempt/session,
intent generation, position, before/after desired-playback state, observed
media state, client timestamp and server receipt timestamp. Bound strings
and reuse existing authentication, redaction and retention. This does not
need a new database or endpoint.

Origins are `viewer_control`, `viewer_keyboard`, `media_session`,
`internal`, and `native_unknown`. Tag commands at the actual entry point.
Carry the ID/reason through the existing queued transport-event marker and
record its matched media event. Internal recovery, source teardown and
exhaustion must say which owner initiated them. An unmatched native event
is `native_unknown`; it must never become a fabricated viewer command.
Reused media elements and late predecessor events retain their original
attempt association. Missing observations remain distinguishable from a
command that was never issued.

**A acceptance:** an injected slow attempt is recorded at authority expiry
before its later timeout and joins to session termination using default
logging; normal
shutdown does not manufacture a failure. A viewer pause, internal exhaustion
pause and unmatched browser pause have distinct persisted origins. The
periodic-work pass (and runner comparison if needed) ends with measured
findings or a precise evidence gap.

## 5. Checkpoint B — correct server lifetimes and ownership

### 5.1 Pause must not turn successful playback into startup failure

Keep first-playlist activation, the 30-second startup budget, 250 ms minimum
presentation progress and the existing 180-second continuous-pause grace.
Preserve generation, owner epoch, producer attempt, accepted sequence and
seek/timeline checks when deciding whether progress is real.

1. Evaluate valid observed progress before discarding a baseline because
   demand changed to Hold. An Active baseline followed by a same-timeline
   Hold report that demonstrates real rendering progress may complete
   startup only when that report still carries `RenderState::Rendering` and
   satisfies the existing progress threshold. Hold is the requested demand;
   rendering is the observed state. A non-rendering Hold does not prove
   presentation; use suspended remaining budget instead. A seek jump, stale
   report or successor cannot prove startup either.
2. If pause arrives before sufficient presentation evidence, retain an
   awaiting-presentation state and suspend its remaining startup budget
   while held. Start finite pause grace on the first accepted Hold even
   before Presented. Repeated Hold heartbeats do not renew that grace.
3. Resume consumes the remaining active startup budget; it does not allocate
   a fresh 30 seconds. Clear/rebase position evidence across seek/attempt
   changes so a discontinuity never proves playback.
4. A held session expires with the pause cause at its grace boundary. An
   active session with no proven presentation still expires on its active
   startup budget. Loss of control lease and authority fencing remain
   independent bounds; pausing cannot preserve an abandoned session forever.

Use the existing actor as the sole owner. Avoid parallel browser/server
startup truth or treating `canplay`, buffer length or HTTP fetches as proof
that frames advanced. Tests must exercise the real actor and competing
expiry ordering, including pre-first-playlist Hold and repeated toggles.

### 5.2 Preserve why a session ended, and reopen only the current paused one

Carry the winning terminal cause through manager control and the existing
HTTP error body, keeping status/code compatibility including
`410 pause_grace_expired`. Durable route rows already carry `terminal_reason`,
and `vodserve::Terminal::{durable_reason,from_durable_reason,control_reason}`
already map its vocabulary. Extend that contract in place where necessary;
do not create a competing same-named enum. Explicitly map the authoritative
`RollingTerminalCause` into applicable wire reasons, preserving existing
`replaced` / `superseded` semantics and unknown old-server outcomes. Do not
add rolling-only lifetimes to VOD semantics merely to share a name. Old
clients can ignore additive response fields. Never infer global quorum loss
from an ordinary session-owner fence.

Retain a definitive terminal fact for the current session/attachment
regardless of whether desired playback is Active or Hold. This includes the
winning cause where known and generic `410 session_ended` from an older
server, with an unknown reason. The reporter stops on a definitive terminal
response, so the transport owner cannot depend on receiving another one
after the viewer pauses. Retain the fact until that attachment is replaced
or closed; an ordinary Pause/Play intent change must not erase it.

Do not stop usable buffered Active playback just to record termination. If
already held, or if the viewer subsequently pauses buffered playback, the
existing transport owner consults the retained terminal fact on Play and
reopens once instead of resuming the dead session. A late response from a
predecessor cannot park or stop its successor; bind the fact to session,
owner and attachment identity rather than whichever intent is current when
the callback arrives. A still-current attachment's definitive termination
must not be discarded merely because its request preceded a Pause command.
Cover both protocol paths: HTTP 410 errors and successful HTTP 200 responses
with `action.type === "terminal"`. The latter currently depends on
`sameIntent`; capture its definitive fact for the same attachment even if
intent changed while the request was in flight. Intent-sensitive media
effects remain with the current owner. Do not relax successor identity checks.
Authentication errors, transient request failures and `owner_transition`
alone are not definitive retirement. VOD/direct-play behavior retains its
own lifetime rules.

On Play, reopen exactly once at the authoritative desired position and
current selection/rate/subtitles, through the existing owner. Active
playback may consume usable buffered media after a server termination;
preserve the surface contract's ownership of when it stops and recovers.

**Temporary fencing is a different control outcome.** The existing serving
middleware returns `503 serving_fenced` with `Retry-After: 1`; the reporter
currently retries `503 control_unavailable` but permanently stops on
`serving_fenced`. Add that specific temporary refusal to the existing
reporter retry/backoff classification. Honor its existing deadline, pacing,
request/capture identity, intent supersession and cancellation rules. Do not
retry every 503 indiscriminately, add a new polling owner or mutate media
from the control reporter.

After authority recovers, a surviving VOD session must resume accepted
control exchanges. A retired rolling session may instead return a
definitive terminal result; retain that fact as specified above and let the
existing playback owner act. Close and attachment replacement cancel the
pending retry. Preserve buffered presentation throughout a temporary
refusal. Segment delivery also renews the VOD sliding TTL, so a stopped
reporter is not evidence of an inevitable 300-second session death.

### 5.3 Sweep previous-process leftovers before admission; release exactly once

`sweep_orphan_dirs` exists for crash/reboot leftovers, but periodic
`DueJob::CleanupTranscode` invokes it against current-process directories.
Its active-only snapshot misses retained sessions. Remove that periodic
orphan scan and perform one awaited startup pass before producers, playback
admission or background media creators can allocate in its directory root.
Verify the actual startup ordering and exclusive process ownership of that
root; a per-node path is not proof that another process cannot still use it.
Preserve any durable/recoverable ownership and existing root/type exclusions.
The pass may delete only proven disposable previous-process scratch, never
VOD cache, Live TV storage or durable recordings.

If startup cannot establish that exclusion, leave ambiguous directories
untouched and report it; resolve that specific ownership issue before
claiming sweep acceptance. Do not silently substitute another periodic
guard/map. A failed startup cleanup is logged with its path category and
can be retried at the next startup; no fresh current-process orphan scans
are scheduled. Leave the other independent cachekeep maintenance in
`DueJob::CleanupTranscode` intact.

After admission, the existing lifecycle/retirement owner alone deletes
current-process session directories. This makes leaks visible as lifecycle
bugs rather than masking them with an orphan scan. Preserve the established
writer barrier, promised-object serve-until deadline and response pins from the
[scratch reservation contract](SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md).

Treat `NotFound` at directory enumeration, per-file deletion and final
directory deletion as successful absence, after the existing writer/promise
conditions are satisfied. Settle scratch release and remove retirement
ownership once. Permission and real I/O failures remain retryable with
reservations retained and bounded log repetition. The startup pass must
finish before any new directory becomes visible; lifecycle cleanup must
neither double-release accounting nor recreate an owned path. No new
all-lifetime directory registry is required.

**B acceptance:** early pause/resume cannot produce the incident's startup
expiry; genuine startup/hold/control expiry remains finite. A terminal paused
session reopens once on Play. A startup barrier test proves no admitted
creator can race the crash sweep; after admission, periodic maintenance
cannot remove retained promises. Externally missing files release accounting
once without an endless retry loop.

Also prove both orderings: terminal while held, and terminal while Active
followed by buffered playback → Pause → Play. A temporary fence must resume
control on a surviving VOD attachment and reach the appropriate terminal
handling on a retired rolling attachment, without a new watchdog.

## 6. Checkpoint C — recovery belongs to a failure episode

### 6.1 Rearm after demonstrated health, not a new attempt ID

Keep one automatic reopen per failure episode. Replace the lifetime meaning
of `stallRecoveries` with explicit episode state; retain a separate cumulative
count only if diagnostics need it. A successful attach, `playing` event,
manual retry or seek does not by itself forgive a failing recovery loop.

The initial policy is **30 seconds of continuous observed presentation** on
the attached attempt, with advancing position and successful control where
control applies. Evaluate monotonic elapsed time using actual observation
ticks; missed/background ticks do not count as proof of continuous health.
Pause, seek, stall, attachment change or terminal error resets the health
streak. After the streak, clear the automatic-attempt budget and log why.
This threshold is a fixed internal policy with deterministic tests, not a
new user setting or claim about a measured optimal value.

Evaluate this state transition in the existing presentation sampling path.
Do not add a `setInterval`, a delayed reopen timer, or a second watchdog to
implement the 30-second streak. A stale/failed control response prevents
the streak from proving health; it does not grant a new recovery owner the
right to interrupt buffered playback.

A second genuine failure after that healthy interval gets one automatic
reopen. A failure before it does not loop. Manual Try again is always an
explicit new bounded recovery request; its success still needs the healthy
interval before automatic rearming. Preserve latest intent/position and
cancel stale timers on replacement or Close. A supply failure must not
automatically force transcoding without decoder evidence.

### 6.2 Every offered action must perform useful work

For this exhausted, terminal stream, offer Try again, the existing explicit
Force transcode choice where applicable, and Close. Remove Keep waiting
when the owner has stopped the attempt or the server has definitively ended
it: there is nothing left to wait for. Keep waiting remains available only
where an existing nonterminal owner can actually continue a bounded wait.
It must not hide the overlay and arm a watchdog that immediately exits.

Update the [surface fixture](../../tests/playback/playback-surface-contract.json)
and [surface contract](../clients/PLAYBACK-SURFACE-CONTRACT.md) in the same
change. Preserve the single presenter and owner-driven effects. Do not make
the reducer manipulate media, and do not add an independent retry loop.
Where the server supplies a verified reason, the UI may explain that the
stream ended; unknown causes must stay unknown. The UI need not expose Raft
implementation details to explain the available action.

**C acceptance:** reproduce first failure → automatic recovery → early
viewer pause → resume → 30 seconds healthy → second independent failure.
There is one automatic recovery for each failure episode, no autoplay
during Hold, no stale overlay and no restart loop. Exhaustion is internally
attributed and every displayed action has an executable transition.

### 6.3 Delivery labels describe the attached session; capability is separate

The current `watchDeliveryRow(f)` maps `f.vod_index_status` directly to a
Delivery badge. It never consults the attached session. The browse endpoint
computes that status through `store.fragment_index`, whose Hiqlite adapter
reads the legacy node-local telemetry index. VOD creation can instead load
the cluster artifact through `try_cluster_fragment_index`. Refreshing the
page repeats this different lookup; it does not establish current delivery.
The legacy store has a row for file 645, but its presence alone does not
prove a match for the current identity. Do not claim the exact legacy miss
was established merely by counting rows.

Use the existing accepted session/attachment descriptor for the watch-page
Delivery row, Playback Info and Activity's delivery vocabulary. Reuse the
existing session state projection; do not make the browser poll Activity,
infer mode from a filename/encoder string, or add a reconciliation watchdog.
During a prepared change, distinguish requested destination from the still
attached presentation. Change the displayed mode when the new attachment
commits. A stale file payload or predecessor response cannot override it.

Keep file analysis/capability separately labeled when no active presentation
exists. Factor the minimum read-only capability resolver shared with VOD
selection's identity rules: source version, pipeline/transform and engine
identity, supported legacy index and cluster artifact availability. Preserve
partial device-route support and unknown/unavailable states. A catalogue
read must not queue a build, hydrate a blob or create a playback session.
Index publication and source changes should invalidate the existing browse
projection through its normal data lifecycle, not a new background loop.

Index completion must not stop, replace or mutate an already attached
rolling presentation solely to change its delivery mode. A normal new open
or explicit retry may choose VOD once eligible. Add a lifecycle regression
for completion during rolling playback and a separate reopen-to-VOD case.
Do not infer that the live stream was switched in place from the later VOD
label. If indexer workload contributed to resource contention, §4.2 must
prove that separately from the index-ready event.

**Acceptance:** with a valid cluster artifact and no matching legacy index,
the server's capability answer is accurate and the active VOD session says
VOD on all three surfaces. A prepared successor, stale catalogue payload,
source replacement and page refresh do not mislabel the attached route.
The completion event alone leaves an existing live-HLS session playing.

### 6.4 Activity must preserve VOD diagnostic meaning without expensive polling

**Additional source-confirmed finding, 2026-10-01 12:47 EDT screenshots.**
Activity obtains VOD identity and byte counters through `VodDeliveryInfo`.
Its adapter, `vod_delivery_session_info`, assigns no reported position,
runway, demand, render state or producer progress, assigns producer `vod`,
and hard-codes `suspended: false`. The shared renderer hides absent values
and falls through to Active. It can therefore disagree with the richer
`VodSessionInfo` already used by Playback Info, including when the producer
is held or failed. Zero placeholders also produce blank advertised-byte
and scratch rows because the byte formatter treats zero as empty.

**Use the existing page inventory, not a selected-session API.**
`/activity/detail` takes no selected stream identity: it returns a page-wide
snapshot and fans out to peers. The small VOD inventory intentionally avoids
rendition manifest locks. Preserve both boundaries. Add cheap observations
to `VodDeliveryInfo` and the existing inventory projection; do not call full
`VodSessionInfo` for each row or invent a selection protocol.

| Fact | Authoritative source / cost boundary | Activity behavior |
|---|---|---|
| Position, demand, render state, client runway | Session's existing `last_control_snapshot`; copy while `delivery_infos_bounded` already holds the sessions lock | Show observed values, including real zero, with observation age; no snapshot means unknown |
| Observation age | Accepted-control update time in [VOD control](../../crates/plurxd/src/vod/serve/control.rs), stamped when a new snapshot is accepted | Expose an optional age; polling, duplicate replay and serialization never refresh it |
| Producer failure | Existing `rendition.failure()` with bounded sanitized reason | Show Producer failed independently of client demand |
| Producer held/running | Existing producer belief/hold state used by Playback Info | Read only after dropping the global sessions lock and bounding candidates; measure cost first |
| Delivery bytes/rate | Existing serving counters | Keep the current measurement/source semantics; distinguish idle from unavailable |
| Manifest-derived counts, ready/fetched frontiers, working-set/cache detail | Rich VOD status owner; may need manifest lock | Leave in Playback Info for this repair |

Copy cheap session observations and the minimal rendition references under
the existing lock, then select/truncate to the existing response limit
before optional producer reads. Do not await `slot.belief()` while holding
the sessions lock or perform expensive reads on rows later discarded.
Retain session/rendition identity through the copy; replacement must not
join one reader's control to another rendition's producer. Producer absence
does not prove completion: if completion needs the manifest, report unknown
or the observed absent state rather than infer Complete. Diagnostics never
renew a lease, produce media or update the observed-control timestamp.

**Peer propagation is an additive contract change.** The existing
`ActivityDelivery` DTO has explicit fields; extending `VodDeliveryInfo`
alone will not reach other nodes. Add optional bounded observations to that
DTO and carry them through the current authenticated activity snapshot and
aggregation. Default absent fields for old peers; old consumers must ignore
unknown fields. Preserve row limits, payload limits, node provenance,
authorization and existing opaque correlation. No new endpoint, selected
session parameter or required peer version is introduced. Verify old/new
serialization both ways rather than claiming there is no wire change.

**Stop presenting placeholders as observations.** The adapter currently
also invents `status_generated_unix_ms: now`, `lease_state: "active"`, zero
progress-idle/fetched-end/staged/advertised/live/http-wait fields and zero
read rate. Snapshot-generation time is not control-observation time; retain
that distinction in names and rendering. Lease presence is not evidence of
viewer playback. Use an explicit VOD presentation rule to suppress
rolling-only placeholders and derive client/producer status from actual
observations. Missing or stale evidence must not fall through to Active.
Keep paused, client stalled/failed and producer failed distinguishable;
producer pacing Hold must not imply a viewer pause.

Several `SessionInfo` fields are non-optional and native clients consume
the `sessions` array. Audit Apple, Android and other actual decoders before
changing any required field's type or meaning. Prefer additive observation
fields and presentation-aware rendering over a blanket numeric-to-null
wire change. Do not relabel rolling scratch as VOD working set or missing
encode speed as zero speed. Fix `fmtBytes` as a focused formatting change:
measured zero renders `0 B`, while absent values remain unavailable/omitted.
Check its existing callers so a placeholder zero is not made to look like a
real measurement. This is a projection repair with no new telemetry loop.

**Concrete regression homes:** add adapter fixtures to
[rolling/session.rs](../../crates/plurxd/src/transcode/rolling/session.rs)
under a local `#[cfg(test)]` module, inventory tests in
[delivery.rs](../../crates/plurxd/src/vod/serve/delivery.rs), peer DTO and
bounded-aggregation cases in the existing tests of
[internal_activity.rs](../../crates/plurxd/src/http/internal_activity.rs),
and rendering cases in [web-policy.test.js](../../tests/playback/web-policy.test.js).
These are tests to build, not claims of existing adapter coverage.

**Acceptance:** local and peer VOD rows agree with Playback Info for their
shared observed facts, including held, paused, failed, stale and no-control
fixtures. Manifest-rich detail may remain exclusive to Playback Info.
Assert no manifest acquisition, no global-lock await, bounded producer
reads, no lifetime renewal, and correct old/new peer behavior. Verify real
zero, unknown and not-applicable separately. Retain live-HLS regressions.

## 7. Regression matrix — pin behavior at the existing seams

The case names below describe **tests to add**, not tests already passing.
Use fake monotonic time and barriers for deadlines/races. Test effects and
ownership through production functions; avoid source-string assertions as
the primary regression. Ensure each new case fails on its corresponding
old behavior before crediting it as evidence.

| Case | Existing test home / required observation |
|---|---|
| Slow in-flight proof versus late sampler versus proof rejection | Tests in [migration.rs](../../crates/plurx-core/src/cluster/migration.rs) and serving fence: expiry precedes timeout and records coherent current phase/age/leader; distinct persisted outcomes, no lease extension, cancellation not failure |
| Leader loss and actual minority partition | Existing [serving fence tests](../../crates/plurxd/src/serving_fence.rs): old generations stay revoked even after rapid recovery |
| Pause with progress before second Active report | Actor tests in [playback_control.rs](../../crates/plurxd/src/playback_control.rs): rendering Hold can prove startup; non-rendering Hold cannot; preserved proof, no startup reap |
| Pause before presentation; repeated holds; resume | Same actor: suspended remaining startup budget, finite 180-second hold, no budget minting |
| Seek jump, stale sequence and changed producer | Same actor: no false presentation or resurrection |
| Terminal reason and mixed-version response | [HTTP control tests](../../crates/plurxd/src/http/hls/control.rs) plus [web-control.test.js](../../tests/playback/web-control.test.js): compatible codes, current-session park, exactly one reopen |
| Terminal before a later Pause or mid-flight intent change | Same web-control harness: both HTTP 410 and HTTP 200 terminal action → buffered playback → Pause → Play reopen once; current attachment fact survives `sameIntent` mismatch, stale predecessor ignored |
| Temporary authority refusal | Same reporter harness: `503 serving_fenced` → accepted control for surviving VOD; fence → terminal for retired rolling; Close/replacement cancels retry; no reporter-driven media mutation |
| Startup orphan sweep versus admission and periodic maintenance | Tests alongside maintenance/retirement and startup scheduling: sweep completes before creation is enabled, previous-process scratch removed, periodic jobs never sweep current-process retained objects, other cache maintenance preserved |
| Already missing directory; real I/O failure | Same cleanup tests: settled once for absence, retained charge for actual failure |
| Second episode versus repeat failure | [web-media-recovery.test.js](../../tests/playback/web-media-recovery.test.js) and [web-policy.test.js](../../tests/playback/web-policy.test.js): rearm only after continuous healthy interval |
| Pause provenance and late media event | [web-control.test.js](../../tests/playback/web-control.test.js) and client-log persistence tests in [system.rs](../../crates/plurxd/src/http/system.rs): matching command, internal reason or explicit unknown |
| Exhaustion actions | [playback-surface-contract.test.js](../../tests/playback/playback-surface-contract.test.js) plus web-control harness: terminal Keep waiting absent, Try again actually opens, Close cancels |
| Cluster index ready during rolling playback | Existing VOD/server fixtures: completion leaves the incumbent alive; a later explicit open chooses VOD |
| Active mode versus file capability | [watch-and-browse.browser.cjs](../../tests/web/watch-and-browse.browser.cjs), browse tests and VOD fixtures: VOD without matching legacy index, stale file payload, pending successor, refresh and source-change controls |
| VOD Activity diagnostic projection | Local tests in rolling/session.rs and vod/serve/delivery.rs; existing tests in http/internal_activity.rs and [web-policy.test.js](../../tests/playback/web-policy.test.js): cheap common-fact parity, hold/failure, actual observation age, peer compatibility, bounded reads, zero/absent rendering and no renewal |

Run each focused set once at its coherent checkpoint; repeat only after a
relevant edit, failure or changed base. Representative existing commands:

```bash
rustup run 1.97.1 rustc --version             # establish the pin before edits
rustup run 1.97.1 cargo check -p plurxd --all-targets --locked
rustup run 1.97.1 cargo clippy -p plurxd --all-targets --locked -- -D warnings
rustup run 1.97.1 cargo fmt --all -- --check

# Core filters must include replicated-store code.
rustup run 1.97.1 cargo test -p plurx-core --features hiqlite-store --lib watermark
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls_startup
node --test tests/playback/web-control.test.js \
  tests/playback/web-media-recovery.test.js \
  tests/playback/web-policy.test.js \
  tests/playback/playback-surface-contract.test.js
python3 -m unittest tests.operations.test_docs_index
git diff --check
```

Add exact filters for the newly named cleanup, transport and persistence
tests; the sample filters above do not claim to select them. Record names
and nonzero executed counts in the PR. Use `make unit-core` or the explicit
`hiqlite-store` feature for focused core evidence. If vendor code changes,
also run its repository compile/Clippy target and affected tests. A source
archive carries no `.git` or credentials when using the
[agent compiler loop](../ci/AGENT-COMPILE-LOOP.md). Reverify the actual intended
base after integration; stale snapshot results are not candidate evidence.

## 8. CI/CD — preserve the fast lane and the quality bar

Verified on the source base above: Forgejo is authoritative; workflow source
is under `.github/workflows/`. The dated corrections at the top of
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) supersede its older
automatic scheduling descriptions. This plan adopts no test waiver from a
different effort's handoff.

1. Keep the PR draft while building. Use normal hooks and pinned compilation;
   run the focused regressions required by AGENTS, without running the full
   workspace suite after every edit.
2. Use `fix(playback): ...` / `fix(cluster): ...` for observable repairs.
   Put one real `Regression-Test: <path>::<test name>` line per named
   regression in the PR body. Keep this document and its index row together.
3. When implementation and focused evidence are complete, obtain exactly one
   independent adversarial implementation review of the complete change.
   Address findings before marking ready. Review the actual race boundaries,
   false startup proof, rearm loops, privacy/retention and stale callbacks;
   require the cause/invariant/owner chain from §1.1 and verify that no new
   watchdog or duplicate authority was introduced. Review delivery identity
   and index-publication lifecycle separately. Do not create a review loop
   for every checkpoint.
4. Mark ready once to start
   [main-fast-lane.yml](../../.github/workflows/main-fast-lane.yml). It already
   owns affected compilation/static checks, Windows compilation when selected,
   workspace Clippy and `make unit` on the pinned FFmpeg 6 surface. Draft
   allocates no jobs; returning to draft cancels the obsolete run. There is
   no additional `fast-lane` opt-in label.
5. Correct blocking failures and rerun affected required checks on the actual
   candidate. Do not bypass red, call a skipped check green, broaden into
   unrelated repairs, or repeat a passed suite without a material reason.
6. Preserve validated regression lines in the landing message. Use
   `python3 -m validation.regression_field --body-file <description>
   --landing-lines`; a Forgejo API merge must put those lines in
   `MergeMessageField`, since its default merge body drops them.
7. Build/deploy through the existing explicit artifact path and record the
   resulting source SHA and image digest. Merge itself does not build or
   deploy. [ci.yml](../../.github/workflows/ci.yml) runs on manual dispatch or
   release tags; [effort-ci.yml](../../.github/workflows/effort-ci.yml) is
   manual. Do not restore automatic full sweeps, runtime schedules or add a
   new qualification bureaucracy for this repair. Existing required checks
   and deployment authorization still apply.

If scope later requires an effort branch, its required development gate and
current-candidate promotion receipt still apply under AGENTS; manual-only
workflows do not waive those obligations. Resolve the actual invocation and
receipt path then rather than pretending an effort PR automatically triggers
an obsolete workflow. Keep this incident repair small enough to avoid that
expansion.

## 9. Fleet acceptance and stop conditions

After the normal reviewed, checked build is available, use the existing
deployment/drain mechanism; do not restart a quorum of voters together or
discard the active viewer's session to obtain evidence. Record rollout and
rollback by artifact identity. No persistent host setting is silently
changed during observation.

One bounded Safari acceptance session on nynuc must cover the Checkpoint C
sequence, the original rolling copy-HLS route, and native-HLS plus hls.js
where the fixtures support both. Use controlled injected failures in the
isolated harness for partition/deadline races; do not deliberately partition
the production cluster. Check that retired session count and charged scratch
return to baseline after their promises expire. Verify retained diagnostics
still explain the injected failure after routine log churn. A lightweight
old-client compatibility check covers the additive server response; this
does not require shipping native-client rewrites.

Keep one compact receipt in this document/PR: base and candidate SHA,
compiler, focused commands/counts/results, review disposition, fast-lane
run, artifact digest, browser route, fleet observation, periodic-work timing,
runner comparison if needed,
and remaining unknowns. Preserve sanitized supporting captures outside the
shared checkout with path and hash; `/tmp` probe files are not durable proof.
No separate status website or parallel plan series is required.

Completion has two explicit meanings:

- **Repair delivered:** all server/web/cleanup regressions pass, causal
  evidence is retained by default, the reviewed current candidate passes its
  required lane, and the bounded playback acceptance succeeds.
- **Incident cause closed:** a trace identifies the failed proof stage and
  its initiating mechanism; a targeted fix or measured runner isolation
  removes that mechanism while real partition fencing still works. If this
  is not yet true, keep it open with the evidence and a single next action.

Stop expanding once those obligations are met. Further player redesign,
general CI capacity tuning and unrelated cleanup belong to separate work.

## 10. Agent and Opus plan reviews — findings and dispositions

**Reviewed:** 2026-10-01 EDT · **Reviewer:** independent adversarial agent
`adversarial_plan_review` · **Scope:** plan against current source and
Forgejo workflows; no implementation review, runtime validation or service
changes. The reviewer returned **request changes**, with two P2 findings
and no other material findings. Both corrections below are now incorporated
in the plan. These are plan reviews, not implementation approval or evidence
that the proposed repairs have run successfully.

| Finding | Evidence / consequence | Disposition |
|---|---|---|
| R1 · P2: terminal state was remembered only while already held | The [control reporter](../../crates/plurxd/src/web/playback-control.js) stops on 410; a later Pause/Play cannot obtain a second terminal response | §5.2 now retains the current attachment's terminal fact independently of intent, preserves buffered playback, and consults it on later Play. §7 adds the Active → terminal → Pause → Play and stale-predecessor cases. |
| R2 · P2: temporary `serving_fenced` permanently stopped control | [Serving policy](../../crates/plurxd/src/serving_fence.rs) and [HTTP middleware](../../crates/plurxd/src/http/mod.rs) provide 503 plus Retry-After, but the reporter's retry whitelist omitted the code; surviving VOD could lose all future exchanges | §5.2 now classifies that response in the existing reporter retry path, with identity, pacing and cancellation preserved and no media mutation. §7 covers surviving VOD, retired rolling, Close and replacement. |

The first review preceded §6.4. Its lack of further findings was not proof
that the plan was complete: Opus found additional material gaps below.

**Opus review:** supplied by Paul; reviewed approximately 16:50 UTC with
Activity addendum approximately 17:15 UTC, 2026-10-01. It checked deployed
Forgejo main `91917940e`, seek branch `589986d5c` and read-only fleet logs.
Verdict: retain the core lifetime/recovery repairs, rewrite the first
diagnostic experiment, simplify cleanup, redesign Activity projection and
record the fencing-policy decision. This revision incorporates the verified
findings; it does not claim Opus has reapproved the revised wording.

| Finding | Disposition / verification |
|---|---|
| F1 · P1: periodic in-process trigger deserves priority over CI | Accepted in §§2 and 4.2. Independently counted 32 nynuc losses and repeated near-16-minute intervals. Reconcile timing is now the first experiment, runner comparison conditional. Qualified the review's exact-period and duration claims: not every loss is periodic, and the full captured duration range is wider. Correlation remains a hypothesis until the delayed stage and initiating operation are traced. |
| F2 · P1: timeout logging happens after authority expiry | Accepted in §4.1. Capture a coherent process-local in-flight attempt at the expiry boundary; link its later timeout by sequence. Add the ordering regression in §7. Lease sizing remains outside this repair. |
| F3 · P2: name the invariant and fence blast radius | Accepted as the explicit open architecture ruling in §1.1. Preserve generation fencing in this PR; permanent teardown versus safely retaining a suspended actor needs evidence about asymmetric proof, takeover and recovery. No claim that simultaneous logged loss makes takeover impossible. |
| F4 · P2: restrict crash sweeper instead of adding a guard | Accepted in §5.3. One startup pass before admission, existing retirement ownership afterward; periodic unrelated cache work preserved. Ambiguous/shared/durable ownership must be excluded, not guessed. No new live-process ownership map. |
| F5 · P2: terminal reason already has a durable vocabulary | Accepted in §5.2. Extend existing reason mappings where needed and explicitly bridge rolling causes; no competing same-named enum or silent VOD lifetime change. |
| F6 · P3: source, terminal-response and rendering corrections | Accepted. §3 includes player.js; §5.2 covers HTTP 410 and HTTP 200 terminal action across mid-flight intent changes; §5.1 retains `RenderState::Rendering`; header names remote main rather than the local-only commit. |
| G1 · P1: no selected-session Activity API exists | Accepted. §6.4 uses the existing page inventory; no selected-session endpoint or peer selection protocol. |
| G2 · P2: expose cheap session/producer facts | Accepted with a wire-contract correction. Copy existing control observations, bound producer reads and avoid manifest locks. The actual `ActivityDelivery` DTO needs additive optional fields for peers; modifying `VodDeliveryInfo` alone does not propagate them. |
| G3 · P2: false freshness, Active and zero placeholders | Accepted. Record actual accepted-control age, suppress rolling-only VOD rows, distinguish unknown from observed zero, audit native consumers and test `fmtBytes` separately. No blanket required-field type change. |
| G4 · P3: concrete tests; separate delivery-truth PR | Concrete test homes accepted in §§6.4–7. Separate-PR recommendation not adopted: shared VOD/control/policy files and the bounded delivery requirement favor separate commits/review sections inside one PR. Rich Activity APIs are excluded to contain scope. |

**Documentation verification:** run `python3 -m unittest
tests.operations.test_docs_index`, validate all relative links in this new
file (including while it is untracked), and run `git diff --check`. These
checks validate the document only. The implementation regressions in §7,
required fast lane and runtime acceptance remain outstanding. No service,
runner setting, fencing rule or source implementation was changed in this
documentation pass.

## 11. Execution instruction — 2026-10-01

Paul superseded checkpoint unit runs: implement in a separate clone, compile,
format and Clippy during development, obtain one adversarial review only after
the complete patch, address it, then run the existing required fast lane.
Add meaningful regressions during implementation; do not repeatedly execute
unit suites. Do not bypass blocking checks. One main-bound PR batches proper
commits; merge when reviewed and green. The original checkout stays untouched.


## 12. Implementation receipt — candidate under validation

**Updated:** 2026-10-01 · **Candidate base:** `91917940e` ·
**State:** complete candidate; one review addressed; required final lane pending.

The separate clone is disposable; this indexed receipt and the one repair PR
are the durable record. No user checkout was changed. No production restart,
deployment, host setting change or fencing-policy relaxation was performed.

| Observed defect | Cause / invariant | Existing owner and correction | Regression intent |
|---|---|---|---|
| Early Hold expires startup | Startup elapsed wall time despite an accepted Hold; evidence and hold lifetime were conflated | Rolling actor suspends remaining presentation budget, accepts valid Rendering progress under Hold, and retains finite 180-second hold grace | `mkv_hls_startup_rendering_hold_proves_progress_but_waiting_hold_does_not`; `mkv_hls_startup_hold_before_playlist_has_finite_nonrenewing_grace` |
| Play resumes an ended attachment | Reporter termination was discarded on intent change | Existing reporter retains definitive 410/200 fact for the captured attachment; transport Play consumes it through the existing seek/reopen owner | Current-owner terminal response cases; `terminal attachment Play reopens once at the paused seek destination` |
| Temporary authority refusal stops VOD reporting | Retry whitelist omitted `serving_fenced` | Existing reporter retries with Retry-After, pacing and identity cancellation | Existing temporary refusal loop now includes `serving_fenced` |
| Separate outage inherits exhausted allowance | Same-file player retained spent count without healthy episode boundary | Existing presentation sampler requires 30 seconds of contiguous actual progress with fresh accepted control before rearming | Healthy episode policy regression; attachment churn and pauses reset evidence |
| Keep waiting clears a stopped prompt | Its detector exits on retired owners | Web exhausted raising sites offer Retry, applicable Force transcode and Close; shared native defaults are preserved because their owners execute bounded retry | `exhausted and stopped prompts offer executable recovery actions` |
| Periodic sweep deletes retirement-owned bytes | Active map omits retained presentations | Reuse `main::run`'s existing locked secure startup scratch clear before admission; remove live periodic orphan deletion, retain cachekeep; retirement alone deletes current-process objects | `rolling_cleanup_missing_directory_is_successful_and_idempotent`; existing protected startup clear tests |
| Pause origin ambiguous | Native event lost original command identity | Existing transport entry points retain bounded command marker through late events; existing playback-event storage persists validated origin and server receipt | `transport_provenance_is_bounded_and_retained_in_playback_events`; late predecessor/native provenance regression |
| Labels disagree with VOD attachment | File capability substituted for attached delivery | Watch ledger reads attachment descriptor; catalogue reconciles legacy and exact attested cluster index metadata | Session-owned watch delivery regression |
| Activity invents health or hides VOD facts | Rolling placeholders substituted for accepted observations | Existing bounded inventory copies optional control facts/age and nonblocking producer belief after selection; no manifest reads, object scans, lease renewal or new endpoint | `status_describes_vod_without_touching_its_idle_lease`; `vod_activity_observations_are_additive_bounded_and_age_in_cache`; `vod_activity_projects_real_control_without_rolling_measurements` |

Native compatibility audit: Swift `PlaybackSessionStatus` uses synthesized
Codable decoding (unknown additive keys are ignored), and Android's existing
`Net` JSON configuration sets `ignoreUnknownKeys = true`. Required existing
session numeric fields and their types remain unchanged; web VOD rendering
suppresses rolling placeholder measurements instead of treating them as facts.
Older peer snapshots omit optional observations and render unobserved.

Initiating quorum cause remains open. Added coherent in-flight request identity
at authority expiry before timeout, accepted/rejected/source-error/timeout/
cancelled outcomes, bounded warning coalescing, leader linearizable-check
latency and reconciliation run/prune/channel/snapshot-fetch/local-evaluation
and existing cachekeep timing. These are passive observations, not a new
watchdog. Remote transport versus apply cannot yet be separated by the opaque
Hiqlite request; leader local check timing distinguishes that stage without
claiming unseen substage durations. The next causal action is to join an expiry
attempt sequence and leader check to the same reconciliation run and phase.
SQL itself executes in the blocking read pool; timing correlation alone is not
proof of Tokio blockage or a reconciliation root cause.

Development evidence: pinned Rust 1.97.1 verified; initial all-target daemon
check passed before edits. Edited workspace all-target check and workspace
Clippy `-D warnings` passed; served JavaScript syntax passed for 70 scripts.
Unit suites intentionally have not run before the independent review, per the
latest user instruction. No repository fail-before/pass-after execution is
claimed; regression intent above is distinct from the isolated incident probes.
Final validation/check/merge and implementation review receipts follow here.


### 12.1 One independent implementation review and dispositions

The independent full-patch adversarial review requested changes for four P2
findings and found no P0/P1 issue or competing lifecycle authority. No unit
suite was executed by the reviewer. All findings are addressed in the candidate:

| Finding | Disposition and regression |
|---|---|
| Peer Activity retained observation facts only in the pill | Normalize the VOD projection once before pill/meters/details; peer-cell regression asserts position, runway, demand, render state, producer hold, age and measured zero |
| Concrete quorum errors lost under `source_error` | Preserve the concrete enum category and a fixed privacy-safe reason; changed outcome/category emits immediately instead of being coalesced with unlike failures; sensitive-payload regression |
| Expiry test checked a hypothetical time at the first request | Paused Tokio clock starts the delayed next sample at 500 ms, advances to the 1-second lease expiry while the 750 ms timeout remains pending, then joins the timeout's completed sequence; existing cancellation test stays distinct |
| Recovery receipt filtered from supervisor logs | Emit bounded recovery summary at WARN; the actual production cluster target is tested through both production console formats under the default INFO filter |

Two additional review findings were fixed during review: ended-toggle harness
now provides the shipped intent dependency; transport late-event regression
runs the actual `clientLog` autofill path and proves a predecessor event cannot
inherit a successor's accepted control, file or session. No additional agent
review campaign was requested or run.

The user requested the required final lane once after review; local final
checks cover only relevant suites absent from that lane. The lane already
executes workspace Rust units, web-policy, web-control and operations contracts.
Failures remain blocking and will be fixed or reported rather than waived.


### 12.2 Final local evidence before the required lane

- `rustup run 1.97.1 cargo check --workspace --all-targets --locked`: passed
  after review fixes (30 seconds).
- `rustup run 1.97.1 cargo clippy --workspace --all-targets --locked -- -D warnings`:
  passed after review fixes; normal commit hooks repeat the same mandatory
  lint/format/syntax policy without executing tests.
- Vendored Hiqlite production-feature Clippy completed in 17 seconds with no
  source lint error. It reports three existing invalid disallowed-method path
  configuration warnings for Tokio process methods not reachable in that
  isolated feature set; workspace Clippy remains clean.
- `scripts/js-check`: 70 served scripts parsed; `scripts/web-types --base
  origin/main`: passed. Exact Player/error declarations lowered the type
  baseline from 524 to 518 diagnostics; no ceiling increase or waiver.
- `node tests/playback/playback-surface-contract.test.js`: all 64 shared cases
  passed. `node --test tests/playback/web-media-recovery.test.js`: 3/3 passed.
  Its first run found a stale literal call assertion which excluded existing
  optional reopen arguments; corrected to assert the forced reopen contract,
  then reran only that failing suite.

Lane-covered workspace units, web-policy, web-control and operations suites
were not duplicated locally. Their actual required fast-lane result remains
blocking. The one PR and its exact-head checks will supply the final receipt;
no local baseline unit campaign or production acceptance is claimed.
