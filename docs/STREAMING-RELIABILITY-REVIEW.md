# Streaming reliability review — keep the stream alive while its future changes

**Reviewed:** 2026-09-04 · **Baseline:** `48615baf` · **Verdict:** the
film-addressed VOD direction is correct, but the shipped system does not yet
meet its no-freeze or transparent-change contracts.

Companion to [PLAYBACK.md](PLAYBACK.md) (current behavior),
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) (immutable presentation),
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
(intended transaction), and
[STREAMING-RELIABILITY-STATUS.md](STREAMING-RELIABILITY-STATUS.md) (live repair
ledger). This document is the evidence-backed review of the last seven days of
changes and the end-to-end streaming path. It is deliberately stricter than a
code tour: if an invariant is present in a type or unit test but cannot be
reached by a real client, it is not counted as a working feature.

## Executive verdict — two good foundations are joined by a missing transaction

The architecture has made two important corrections:

- immutable, film-addressed VOD removes mutable-playlist, moving-window, and
  restart-at-an-earlier-keyframe failure classes for copy/remux playback; and
- the control protocol gives one actor a typed, sequenced view of client
  demand, delivery state, producer state, and intended selection.

The gap is between those foundations. Session creation prefers VOD, but VOD
still refuses every transcode rendition and burned-subtitle stream. The shipped
default Cargo feature and runtime default then silently route those refusals
through the retained growing-live engine, contradicting the published cutover.
The control plane can record a proposed successor, but it neither prepares
playable media nor sends, acknowledges, commits, or rolls back a replacement
action. Every client still destroys or detaches the working stream during
important changes. Two client recovery loops also have paths where a permanent
freeze cannot cause a recovery.

That is not a tuning problem. Increasing timeouts, changing buffer sizes, or
adding another retry will not complete a transaction that has no reachable
commit. The repair needs to finish the VOD coverage and replacement lifecycle,
then make the clients use it.

### Priority summary

| Priority | Count | Meaning |
|---|---:|---|
| P0 | 4 | A viewer can remain frozen indefinitely, or a core playback class cannot be served |
| P1 | 13 | A transition can lose position, choose stale state, poison a rendition, or interrupt healthy playback |
| P2 | 10 | Tests, operations, telemetry, or enablement can certify or present the wrong thing |
| P3 | 3 | Input, protocol-conformance, and documentation drift invite the next defect |

No finding recommends keeping growing live HLS as a production fallback. The
target makes immutable VOD the whole segmented media plane and makes control an
actual transaction rather than a second session-start hint channel.

## Scope and method — broad history, narrow claims

The review window begins at local midnight on 2026-08-28 and ends at baseline
`48615baf` on 2026-09-04. It contains:

| Measure | Value |
|---|---:|
| Commits reachable from `main` | 1,193 |
| First-parent changes | 217 |
| Changed files | 512 |
| Insertions / deletions | 220,101 / 50,655 |
| Playback-focused files sampled in depth | 140 |
| Playback-focused insertions / deletions | 103,516 / 33,872 |

The volume is too large for a useful claim that every added line received the
same attention. The review instead followed each user-visible operation across
its whole path and sampled every changed subsystem that can alter it:

1. web, Apple, and Android player state and recovery loops;
2. session decision and creation, VOD attachment, segment demand, and producer
   lifecycle;
3. control exchange, sequence and owner fences, action selection, preparation,
   acknowledgement, commit, abort, and replay;
4. cluster placement, durable routes, fragment indexes, storage waits, and
   owner loss;
5. settings, operator status, metrics, telemetry, CI, nightly validation, and
   physical evidence;
6. de-identified seven-day telemetry and post-deploy logs on `nynuc`, `m6`,
   `nuc3`, and `nuc4` using the supplied read-only deployment access.

Three independent passes attacked the server, clients, and operations/test
contracts. A fourth exact-head adversarial pass is required on every repair PR
before merge. Findings below distinguish source proof, fleet observation, and
inference; none promotes one into another.

## Current architecture — the old stream stops before the new stream exists

### Ordinary VOD playback

```text
client decision
      |
      v
POST HLS session ----> durable route ----> VOD rendition ----> fragment index
      |                                      |                       |
      |                                      +---- demand ----------+
      v
immutable playlist --> init + segments --> player buffer --> decoded frames
```

This path is sound when the selected recipe is copy/remux, the fragment index
exists, and the VOD owner survives. A seek is then a request for another point
on the same immutable timeline. It needs no new session and should preserve the
decoder, buffer, and playback identity.

### Quality, recipe, and recovery changes today

```text
viewer / ABR / watchdog
      |
      v
stop, release, or detach current player
      |
      v
create replacement session --> wait for playlist/init/media --> attach player
      |
      +--> failure now has no known-good stream to return to
```

The details differ by client, but the invariant does not: web tears down HLS
before opening the successor; Apple pauses one player and replaces its item;
Android disposes the controller/player around a new plan. The interruption is
therefore designed in.

### Control preparation today

```text
accepted selection change
      |
      v
detached server task --> durable "prepared" row + placeholder StartResponse
      |
      +--> no media producer, no priming, no prepare action to client
      +--> client vocabulary excludes prepare_replacement
      +--> every acknowledgement is rejected
      +--> deadline eventually aborts bookkeeping
```

The `ControlAction::Prepare` type and acknowledgement states describe the
right future protocol. They do not form a reachable production state machine.

## Findings — ranked by user-visible failure

### P0-1 — Android cannot detect an open-ended buffering freeze

`BufferingStallTracker` emits a stall measurement only when buffering ends or
the playhead moves again. `Controller.onStall` explicitly records that an
open-ended freeze cannot reach it. The control exchange, one-rung-down reopen,
and terminal budget are consequently unavailable in the failure that most
needs them.

**Impact:** an Android/Google TV player can remain on a frozen frame forever
without producing the action that would reconnect it.

**Required correction:** add an independently scheduled, monotonic no-progress
deadline while `playWhenReady` is true. It must fire during buffering, be
cancelled by progress/pause/seek/generation change, ask control within a short
bound, and then perform one fenced same-delivery recovery. Keep the recovered-
stall measurement as telemetry; it cannot own recovery.

### P0-2 — web and Apple decoder freezes can accept `hold` forever

The Apple recovery monitor correctly fires on a stagnant clock even when
AVPlayer claims it is playing. But `holdMayDecideStall` returns true for every
`.silent` freeze. `applyStallVerdict` then shows a transient notice, restarts
polling, records `server_hold`, and returns before the HDR/same-delivery recovery
path. The monitor re-enters and can accept the same hold again without a retry
budget or terminal bound.

This matches historical fleet evidence on `m6`: one VAAPI playback repeatedly
reported a frozen position, about 61.5 seconds of runway, `server_hold`, and
increasing stall durations for many minutes. Across the seven-day node-local
sample, `m6` held 642 stall records over 156 sessions, with a maximum reported
stagnation of 896,194 ms. Those records span multiple builds, so they prove the
failure class in operation, not that every event came from the current binary.

The web path has the same shape. A persistent decode stall that receives
`hold` schedules another wait without applying the finite defer limit used by
`retry_resource`. It can therefore circle through the same frozen state with
neither a recovery token nor a terminal deadline.

**Impact:** server production state vetoes decoder recovery even though a
healthy buffer and a frozen clock are evidence of a decoder/item wedge, not a
need for more produced bytes.

**Required correction:** no advisory hold may be terminal authority over a
stagnant renderer. A hold can delay producer work, but the client recovery
owner needs a finite deadline after which it reattaches the identical delivery
or tries the compatible decode/HDR fallback. Prove the whole loop, not only the
pure `holdMayDecideStall` predicate.

### P0-3 — transcode and burn silently fall back to the old mutable stream

VOD creation returns `vod_transcode_unavailable` for transcode rungs and rejects
bitmap subtitle burn. These are not edge cases: unsupported source codecs,
remote bandwidth, quality selection, HDR conversion, and burn-only subtitles
all require a different recipe.

The claimed cutover is not the shipped behavior. `plurxd` enables the
`live-hls-recovery` Cargo feature by default, and production treats a missing
runtime setting as enabled. VOD index, transcode, burn, and source refusals can
therefore enter the retained growing-HLS engine. Tests invert the production
default and require an explicit `1`, so ordinary refusal tests do not exercise
the deployed configuration.

**Impact:** the playback classes that most need buffering and immutable
addressing are exactly the classes routed back to the mutable playlist/session
architecture implicated in freezing and restart discontinuities.

**Required correction:** implement immutable, on-demand transcode renditions
and burn renditions over the existing fragment plan. A segment number must
always name the same film-time interval across every rendition, and a far seek
must materialize only bounded work around the requested interval. Remove the
Cargo feature split and production-default fallback. Until recipe coverage is
complete, any emergency live recovery is an explicit Developer enablement with
its freeze/seek costs and production-equivalent tests, never a silent default.

### P0-4 — prepared replacement has no reachable commit

The server stages only after sending the accepted control response. The staged
row contains a placeholder response (`encoder = "staged"`, empty ladder) and
no playable media is primed. The emitted response remains `action: none`.
Web, Apple, and Android advertise only `hold`, `retry_resource`, and `terminal`;
Apple advertises dual-player preparation despite not accepting the prepare
action. The HTTP handler rejects every acknowledgement as stale, and no CAS
moves authority from predecessor to successor.

**Impact:** neither manual quality changes nor automatic adaptation can be
transparent. The current implementation consumes resources and writes state
that no client can use.

**Required correction:** finish one transaction end to end before expanding
axes: stage a real media successor, prime init plus platform-specific runway,
emit a fenced prepare action, accept monotonic readiness acknowledgements,
commit authority atomically, switch the client, retain predecessor readers,
and abort only the successor on any pre-commit failure.

### P1-1 — segment wait rejection can poison an entire VOD rendition

A missing segment arms the rendition-wide materialization watchdog before
wait-pool admission. Busy/refused waits and disconnected request futures remove
their wait-pool state but do not clear the rendition's sticky `demand_since`.
When the watchdog expires it records a whole-rendition failure. A seek storm can
therefore admit only the bounded number of waits while abandoned, unadmitted
targets retain enough state to kill the rendition roughly one deadline later.

The existing seek-storm unit exercises `WaitPool` directly. It proves the cap
but bypasses the watchdog side effect outside the pool.

**Required correction:** use a demand ownership token spanning admission,
disconnect, supersession, and watchdog settlement. Arm only after admission;
cancel when the last live demand for that generation leaves; fail a rendition
only for a deadline still owned by current demand.

### P1-2 — VOD producer failure is erased before action selection

`vodserve` exposes a typed rendition failure, but `DeliveryView::from_status`
sets the VOD `producer_decision` to `None`. Segment requests can be returning a
typed 5xx while control continues to answer `action: none`.

**Impact:** clients spend generic watchdog/reopen budgets instead of receiving
the server's authoritative retryable or terminal reason.

**Required correction:** map VOD failure classes into the same bounded producer
decision vocabulary as rolling delivery, with tests from rendition failure
through serialized control action.

### P1-3 — preparation is unreachable on the production VOD observation

Preparation requires a throughput headroom proof. The VOD delivery view always
reports `delivered_bps: None`, so the headroom decision refuses with
`throughput_unreported`, including for the Apple cohorts that advertise
dual-player preparation.

**Impact:** even a completed client action handler would see no production
prepare decision on the primary presentation.

**Required correction:** measure delivered bytes over monotonic time at the
serving boundary and carry the result into the VOD control view. Unknown must
stay unknown; it must not be replaced by the configured encoder target.

### P1-4 — detached preparation can stage a stale selection

The detached staging task retains no accepted control sequence or desired-
selection revision. Rapid A → B → C changes can race store reads; whichever
task claims the single preparation slot first can stage B after C has already
become authoritative.

**Required correction:** bind `action_id`, accepted sequence, predecessor
generation, and full desired-selection digest into the slot CAS. Re-check all
four immediately before publication and before commit. A newer accepted
selection aborts older uncommitted work.

### P1-5 — commit can succeed after the preparation deadline

The store commit path does not compare the current time with the durable
preparation deadline. Timer-driven abort is asynchronous, so a delayed client
acknowledgement can win the race and commit work that policy already considers
expired.

**Required correction:** deadline validation belongs inside the same atomic
commit operation as predecessor/action fencing. A timer is cleanup, not
authority.

### P1-6 — the planned resume frontier can skip unseen film time

Staging derives its resume point from the predecessor's fetched-through segment
end. Fetch completion is not rendered position. Clients prefetch, retry, and
seek out of order; after a back-seek the fetched frontier can be far ahead of
the actual playhead.

**Impact:** a nominally transparent handoff can jump forward and discard film
the viewer has not watched.

**Required correction:** choose the switch boundary from the accepted client
position/seek target plus a small aligned lead, constrained by contiguous
buffer evidence. Never use a high-water fetch frontier as presentation time.

**Corrected — the anchor.** `stage_prepared_successor` no longer computes
`media_origin_ms + fetched_through_ms`. The accepted envelope's
`seek_target_ms.unwrap_or(position_ms)` is captured at the control seam,
carried through `PreparationCandidateInputs`, and persisted as the successor's
absolute film time with the origin *not* added back. Proved at the helper and
at two real `control_local_inner` exchanges — the arrangement separates all
four candidate answers, so no assertion passes for the wrong reason — and each
of the three tests fails against the previous formula.

**Still open — the lead.** This closes only "never use a high-water fetch
frontier as presentation time". The *small aligned lead constrained by
contiguous buffer evidence* is not implemented: a successor staged exactly at
the playhead is behind the viewer by the time it commits, and choosing the lead
needs `buffered_from_ms`/`buffered_through_ms` contiguity, which is a policy
decision this correction deliberately did not invent. Note it cannot be applied
where the resume is chosen: `stage_prepared_successor` knows neither the elapsed
stage-to-commit time nor the buffer's contiguity, so the lead belongs at or
after commit.

**Still open — the successor's origin field, found by the adversarial review of
the correction above.** Staging writes `resume_ms` into the successor's
`media_origin_ms` as well as its `start_seconds`. Every public session is
`Presentation::Vod`, whose timeline is the whole film from zero — `try_vod_session`
returns `media_origin_seconds: 0.0` — so a VOD successor's true origin is `0`
and the resume belongs only in the recipe's `SessionRequest.start_seconds`.
Readers that treat `media_origin_ms` as an origin then double-count:
`owner_loss_resume` and `takeover_resume` compute
`media_origin_ms + fetched_through_ms`, and a VOD route's `fetched_through_ms`
is already absolute, so a viewer 40 minutes in whose owner is lost at 45 minutes
would be reopened around 85 minutes. The client contract has the same shape —
"align the second timeline with `media_origin_ms`".

This predates the resume correction and is *not* a regression from it: the old
frontier value was strictly larger. It is unreachable today because no shipping
client names `prepare_replacement` in `supported_actions`, so nothing commits a
staged successor — but it becomes live the moment one does, which §4 requires.
Fix it with the prepared transaction, either by writing `0` for a VOD
successor's origin or by stopping `owner_loss_resume`/`takeover_resume` deriving
absolute positions from it, and prove it by committing a successor and asserting
the film position a client is handed.

### P1-7 — ordinary changes remain break-before-make on every client

The web quality, Auto, seek/reopen, and rejection paths tear down HLS before
the replacement opens. Apple pauses, creates, deletes, and replaces one item.
Android clears plan/controller/player state around a new plan. Failure cannot
leave the predecessor playing because it has already lost ownership locally.

**Required correction:** all recipe changes enter one client handoff state
machine. The current player stays authoritative until successor metadata and
minimum buffer are ready. Seek on immutable VOD is excluded because it changes
only the position in the current item.

### P1-8 — VOD owner loss is intentionally unrecoverable

The current VOD path treats owner loss as terminal because materialized bytes
and in-process rendition state are node-local. Durable routes and immutable
names do not make the media available on another node.

**Required correction:** define an owner-loss action that creates or hydrates
the identical rendition on an eligible node at the same film-time boundary.
This can be a prepared replacement; it cannot be a silent URL mutation. Until
that exists, Developer enablement must say that node loss interrupts VOD.

### P1-9 — serving-authority sampling can interrupt healthy playback

Serving authority refreshes every 500 ms, permits 750 ms for an observation,
and expires after one second. Sampling is sequential, and a failed observation
leaves the prior watermark to expire. There is no enforced invariant that the
lease exceeds refresh plus timeout plus scheduling jitter. The serving fence
also requires exact apply lag zero and returns 503 while unready.

Both recorded VOD acceptance failures coincided with authority expiry. A
single delayed observer is not evidence of split brain, yet it can currently
fence media reads and terminate mutable sessions.

**Refinement from the integrated-source audit:** new GETs/publications and
mutable producer work are fenced; already-open exact HLS segment bodies can
drain within their existing no-progress and maximum-body deadlines. Their EOF
bookkeeping rechecks the authority generation and discards stale completion.
The problem is not that every in-flight immutable body is immediately killed.
Raw direct-file bodies are lazy, drop-owned file streams; this pass has not
demonstrated an orphan requiring an HLS-sized total-transfer timeout, which
would itself break legitimate long progressive playback.

Two distinct liveness defects need separate proof. Sequential sampling must
budget `max(refresh, previous sample duration) + next sample duration + jitter`
inside the lease; simply comparing the 500 ms refresh with a one-second lease
misses consecutive valid 750 ms observations. Separately, replacing a still-
eligible proof with a newer committed index before local apply catches up can
create an irreversible serving-loss generation. Retain eligible serving
evidence only until its **original** request-start expiry, while preserving
the latest observed watermark/apply lag as truthful diagnostics. Any such
projection must invalidate on term, leader, or observation-epoch change and
must not change bounded-replica-read semantics. The previous release's
1,500 ms election floor remains a mixed-version constraint; the current
2,400 ms floor is not by itself permission to lengthen the lease.

**Authorization status:** the independent adversarial design review accepted
the bounded eligible-proof approach, but automated safety review refused the
source edit because it changes quorum-serving authority. The isolated
`codex/serving-proof-continuity` worktree remains unchanged at `56e4f6eb`.
Implementation requires explicit approval; no alternate write path, lease
change, production setting, or deployment was attempted. Unaffected client
and media-production tasks continue.

**Required correction:** separate inability to refresh an observation from a
confirmed term/quorum change. Refuse new mutations and publications when
authority is uncertain, but allow already-admitted immutable bytes to drain.
Enforce the timing invariant, expose fence reason/duration/impact, and inject
delayed-watermark and real term-change cases.

### P1-10 — the playback lab discards the transition it is meant to measure

After each operation the lab replaces its baseline with a fresh snapshot, then
scores only the interval after that snapshot. A freeze, hitch, or black period
during seek/audio/subtitle transition is erased. The manifest has no manual
quality down/up operation at all, and short post-operation windows cannot prove
the 60-second ABR dwell or multi-minute preparation deadline.

**Required correction:** keep the pre-operation counters and wall/media clocks,
score the transition itself, and add manual down/up, Auto cliff, preparation
timeout, and leak checks. The assertion must fail if a requested case was
skipped or never reached playback.

### P1-11 — VOD uses historical fetch maxima as current seek demand

The reader frontier is a monotonic high-water mark. The driver schedules work
from it, and eviction protects one continuous range from just before the last
served segment through that old high frontier plus the ahead window. A viewer
who seeks one hour forward and then back to five minutes can therefore keep
production aimed near one hour and protect most of the intervening title from
eviction. The working set can reach `NoRoom` while the player waits at five
minutes.

The wait pool compounds this by exposing only the lowest blocked segment to the
driver. During a scrub, a stale low request can win while the newest desired
target is refused by the cap. Its limits are also hard-coded at four waits per
session and 64 per node despite the VOD plan promising a configurable global
cap; sixteen maximally blocked clients can make every request from the
seventeenth fail immediately. Existing tests prove a forward target and pool
limits separately; they do not prove latest-target service after a reversal or
fair service at the global limit.

**Required correction:** split historical delivery telemetry from current
demand. A large accepted discontinuity creates a new demand generation and
window; exact in-flight requests retain their own pins, while superseded waits
leave. Schedule the latest per-playback target with bounded fairness rather
than collapsing every waiter to a global minimum.

The Astra implementation review adds an important capacity boundary: fair
selection is not a real-time guarantee. One producer per rendition can still
spend its budget repeatedly repositioning between distant active viewers.
Qualification must exercise that demand spread. Where a single producer
cannot maintain every admitted viewer's runway, use bounded independently
scheduled producer windows or refuse excess work explicitly; a FIFO unit
regression alone does not prove multi-viewer playback quality. Terminal session
detach must also retire that session's waits and materialization clocks, so a
departed viewer cannot remain a competing foreground demand until HTTP timeout.

### P1-12 — `NoRoom` can be returned as an indefinite advisory hold

The scheduler emits `NoRoom` when blocked demand exists and no eligible segment
can be evicted. VOD publishes it as a producer hold. At an unmaterialized seek
target there may be no ready-ahead frontier, so control sends `hold` and the
client waits even though the producer has already said waiting cannot create
space.

**Required correction:** classify working-set exhaustion as bounded resource
recovery, retire stale demand generations, and return a retryable/terminal
decision if space cannot be made. A condition that needs external state change
is not a healthy pacing hold.

### P1-13 — clients do not preserve an authoritative pending seek target

The future transaction depends on accepted playhead/seek evidence, but Android
infers seeking only while Media3 is buffering and `currentPosition` differs
from `contentPosition`; ordinary same-content seeks generally keep those equal.
It then reports the current position rather than a separately retained target.
Web clears its preview before invoking `seekTo`, and the non-VOD path begins
destructive teardown without first publishing an urgent seek snapshot.

**Required correction:** retain an explicit pending target above player/item
lifetime, publish it before media mutation, coalesce by control sequence, and
clear it only when the new position is rendered or the intent is superseded.
Rapid seeks must publish and execute only the final target.

### P2-1 — Android reports the wrong selection identity

Android creates sessions using the current quality preference, but its control
snapshot always reports `QualitySelection.Auto`. Rebuilding the controller also
creates a new `playbackId`, so the server cannot reliably relate the requested
change to the active playback.

**Required correction:** playback identity belongs to the player presentation,
not a controller instance. Report the actual manual/Auto selection from the
same source used to construct the session request.

### P2-2 — the protocol enablement copy describes obsolete behavior

The Developer page says control is passive, measures a quality change, and
takes no automatic action. Current source emits advisory hold/retry/terminal
actions, while preparation remains incomplete. The underlying setting defaults
off when absent. The page neither states prerequisites nor distinguishes safe
advisory use from unsafe prepared replacement.

**Required correction:** make Developer settings the explicit enable surface
the operator asked for. Show current readiness per server/web/Apple/Android,
required physical evidence, known limitations, restart/app-version scope, and
the exact behaviors activation enables. Do not add compile-time gates or
secret secondary flags.

The same disposition applies to two adjacent gates. The default-off typeless
sliding setting exists because growing EVENT playlists can change semantic
shape under one URL and stall AVPlayer after pruning; it must be qualified and
graduated or removed with the live fallback, not left as a permanent hidden
mitigation. `playback.auto_abr` is also default-off and implemented only by
web; it moves into the same readiness surface until all installed clients have
an executable adaptation owner, then graduates rather than remaining a
forever-preview switch.

### P2-3 — capability advertisement contradicts executable vocabulary

Apple reports `dualPlayerPreparation: true`, based on physical measurements,
while its action decoder and tests reject `prepare_replacement`. Elsewhere,
comments still claim every client reports false.

**Required correction:** separate measured device capability from installed
protocol support, and advertise true only when both are true for the specific
recipe/device cohort. A server must never infer action support from a transport
capability boolean.

### P2-4 — the strongest VOD regressions are intentionally absent from CI

The pull-request VOD browser job runs only `suspend-resume`. Steady playback,
the 20-seek storm, and stall recovery are explicitly excluded. The seek-storm
unit bypasses the real rendition watchdog, which is exactly where P1-1 lives.
Recent nightly runs also fail before playback when Chrome cannot be found, so
their existence is not evidence that playback ran.

**Required correction:** run steady, seek-storm, and stall-recovery against the
real HTTP/VOD path in the PR or promotion gate; make browser installation a
preflight with a typed infrastructure failure; retain media/log artifacts; and
require the nightly playback stage to have actually started before reporting
the workflow as useful evidence.

### P2-5 — in-memory counters lose the evidence needed for rollout

Control and preparation metrics reset on every daemon restart. The fleet was
deployed at `03379035` on all four nodes during this review; post-restart
control/preparation counters were all zero, which cannot distinguish no
traffic, disabled advertisement, and an unreachable action without joining
other evidence.

**Required correction:** persist a bounded action/outcome ledger in the
existing telemetry store, keyed by build, platform, action, refusal, and
terminal phase. Metrics remain the live view; the ledger answers rollout and
regression questions across restarts.

### P2-6 — deployment health can invalidate playback assumptions silently

The four nodes ran the same source build, but usable pipeline evidence differed
by node. NVENC was unavailable everywhere; QSV/VAAPI validation succeeded for
basic paths, while Main10/HDR graphs failed or were refused on some nodes.
`nynuc` also had hundreds of fragment-index requests waiting and dozens at the
attempt limit during the audit. Those facts directly affect recipe placement,
VOD availability, and successor preparation.

**Required correction:** the readiness model must join media-plane
prerequisites, not merely daemon health: exact build, fragment-index coverage,
cache/scratch headroom, encoder/grade matrix, control action support, and recent
end-to-end acceptance. Placement must not choose a node that cannot build the
promised rendition.

### P2-7 — terminal verdict lifetime differs across clients

Web clears an armed terminal verdict whenever a replacement session attaches.
Apple and Android preserve it across a same-title reopen and clear it only on a
new title or release. A web replacement can therefore lose the server's final
diagnosis before the successor failure it was intended to explain.

**Required correction:** specify action lifetime by playback identity,
selection/intent generation, and server lease, then run the same vectors on all
three clients.

### P2-8 — owner change adds the full control wait to a frozen viewer

An urgent ask pins a sequence floor under the old owner. On `owner_changed`,
reporters reset their sequence for the new owner, so the old ask cannot be
satisfied and waits for its 1.5–3 second cap before legacy recovery proceeds.

**Required correction:** settle the current ask immediately with no verdict
when ownership changes. Reporter adoption of the new owner proceeds
independently, and no old-owner action may cross the epoch.

### P2-9 — malformed and unsatisfiable byte ranges become full-file responses

The direct-play Range parser returns the same `None` for a missing header, a
malformed range, and an unsatisfiable range. The serving path turns every
`None` into a full `200`, and `len - 1` underflows for an empty file.

**Impact:** a bad or out-of-range media retry can unexpectedly transfer the
whole file instead of receiving `416 Range Not Satisfiable`; zero-length media
can panic or construct an invalid bound.

**Required correction:** parse into `Missing`, `Valid`, and `Unsatisfiable`,
return `Content-Range: bytes */len` with 416 where required, and cover empty,
suffix, overflow, and malformed ranges.

**Corrective task:** [Forgejo #14](http://192.168.4.7:3000/noirr/plurx/pulls/14)
at exact-approved `9239999a` implements the parser and actual-response tests,
HEAD method handling, conservative If-Range fallback without a strong current
validator, and successful-GET-only playback bookkeeping. The bounded list is
validated before choosing its first satisfiable range. Independent review
caught two edge cases before approval: descending numbers that both saturate
to `u64::MAX`, and a positive suffix on an empty representation. The latter
returns an empty 200, consistent with
[RFC 9110 byte ranges](https://www.rfc-editor.org/rfc/rfc9110.html#section-14.1.2),
without constructing a negative end. All 45 focused stream tests pass.

### P2-10 — wait capacity is neither configurable nor attributable

The hard-coded 64-node/four-session cap has no Developer setting, per-playback
fairness view, or rejection attribution. An operator cannot tell whether a 503
came from one seek storm, many healthy viewers, abandoned demand, or a budget
appropriate for a different node.

**Required correction:** expose bounded wait capacity beside its memory/file-
descriptor cost, record live/admitted/refused waits by reason without unbounded
labels, and schedule one current demand per playback before admitting a second
from another.

### P3-1 — client snapshot validation is weaker than the server contract

The server requires every non-off subtitle mode to name a track. Apple and
Android enforce only the inverse (`off` has no track); web is looser still. A
malformed transient snapshot receives a non-retryable 400 and can permanently
stop reporting.

**Required correction:** generate shared protocol-conformance vectors from the
wire contract and execute them against Rust, web, Apple, and Android.

### P3-2 — status and code comments materially misstate live behavior

Examples call preparation passive or say nothing is staged while production
does write a durable staged row; comments say every client advertises no
preparation while Apple says true; status refers to a
`playback.prepared_handoff` setting that does not exist; and the HTML status
describes the live recovery that the VOD cutover document incorrectly claims
was removed.

**Required correction:** keep one generated capability/readiness matrix as the
current truth and archive milestone chronology as history. Operator-facing
status must be updated by the same PR that changes behavior.

### P3-3 — progressive pacing accepts non-finite input

The progressive stream pace parser rejects negative values but accepts
positive infinity, which can be passed into ffmpeg argument construction.

**Required correction:** accept only finite values within an explicit sane
range, with zero retaining its documented unpaced meaning.

## Finding evidence index — baseline source and retained observations

Links and symbols below name the reviewed `48615baf` implementation. The
de-identified commands, fleet aggregates, deployed build, and nightly run IDs
are retained in
[evidence/STREAMING-RELIABILITY-2026-09-04.md](evidence/STREAMING-RELIABILITY-2026-09-04.md).

| Finding | Source or executable evidence |
|---|---|
| P0-1 | [`BufferingStallTracker.sample`](../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackTelemetry.kt#L265), [`Controller.onStall`](../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt#L931), and `PlaybackTelemetryTest`'s threshold-without-result case |
| P0-2 | Web `persistentWait` hold branch in [`index.html`](../crates/plurxd/src/web/index.html#L3614); Apple [`applyStallVerdict` and `holdMayDecideStall`](../clients/apple/Sources/PlayerController.swift#L3557); `AppleClientTests.testAHeldWedgeSpendsNothingAndSaysNothingSoTheReopenCanAnswer`; retained `m6` aggregate/detail query |
| P0-3 | Default feature in [`crates/plurxd/Cargo.toml`](../crates/plurxd/Cargo.toml#L9); production/test default split in [`TranscodeManager::live_hls_recovery_enabled`](../crates/plurxd/src/transcode.rs#L13570); VOD refusal in [`VodServe::try_create_with_release_fence`](../crates/plurxd/src/vodserve.rs#L2307) |
| P0-4 | Response-before-stage in [`control_session_local`](../crates/plurxd/src/http/hls.rs#L5116); placeholder stage in [`stage_prepared_successor`](../crates/plurxd/src/http/hls.rs#L5549); client vocabularies in [`playback-control.js`](../crates/plurxd/src/web/playback-control.js#L14), [`PlaybackControlReporter.swift`](../clients/apple/Sources/PlaybackControlReporter.swift#L316), and [`PlaybackControlReporter.kt`](../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlReporter.kt#L335); [M6 caller handoff](M6-CALLER-HANDOFF.md) §3.4–3.5 |
| P1-1 | Watchdog/frontier before admission in [`VodServe::segment`](../crates/plurxd/src/vodserve.rs#L4112); pool cancellation in [`waitpool.rs`](../crates/plurxd/src/waitpool.rs#L126); sticky failure in [`vodserve.rs`](../crates/plurxd/src/vodserve.rs#L4299); isolated `WaitPool` storm test at `waitpool.rs:405` |
| P1-2 | VOD mapping in [`DeliveryView::from_status`](../crates/plurxd/src/playback_control.rs#L836); action resolution at `playback_control.rs:1882`; VOD failure status at [`vodserve.rs`](../crates/plurxd/src/vodserve.rs#L3315); HTTP 502 mapping at `http/hls.rs:8851` |
| P1-3 | `delivered_bps: None` at [`playback_control.rs`](../crates/plurxd/src/playback_control.rs#L847); `has_throughput_headroom` refusal at `playback_control.rs:1120`; impossible injected test rate at [`http/hls.rs`](../crates/plurxd/src/http/hls.rs#L13727) |
| P1-4 | Detached spawn at [`http/hls.rs`](../crates/plurxd/src/http/hls.rs#L5181); asynchronous candidate reads at `http/hls.rs:5409`; preparation slot identity at [`playback_control.rs`](../crates/plurxd/src/playback_control.rs#L2418) |
| P1-5 | SQLite commit at [`sessions.rs`](../crates/plurx-core/src/store/sqlite/sessions.rs#L1623), Hiqlite commit at [`hiqlite_sessions.rs`](../crates/plurx-core/src/store/hiqlite_sessions.rs#L1750), and best-effort timer at `http/hls.rs:5707` |
| P1-6 | Resume calculation in [`stage_prepared_successor`](../crates/plurxd/src/http/hls.rs#L6459) and its capture in [`PreparationCandidateInputs`](../crates/plurxd/src/http/hls.rs#L6239); fetched-end caveat in [`media_sessions.rs`](../crates/plurxd/src/media_sessions.rs#L2497); anchor proved by `a_staged_successor_resumes_at_the_accepted_playhead_not_the_fetch_frontier`, `the_control_seam_stages_from_the_accepted_playhead_not_the_route_frontier` and `a_backward_seek_stages_from_the_seek_target_at_the_control_seam`; aligned lead still unimplemented |
| P1-7 | Web teardown in [`play`/`seekTo`](../crates/plurxd/src/web/index.html#L7807); Apple pause/replace in [`PlayerController`](../clients/apple/Sources/PlayerController.swift#L2601); Android reload/dispose in [`PlayerScreen`](../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt#L469); predecessor Store end at `sessions.rs:923` |
| P1-8 | Takeover exclusion in [`media_sessions.rs`](../crates/plurxd/src/media_sessions.rs#L2450); 410/reopen response in [`http/hls.rs`](../crates/plurxd/src/http/hls.rs#L8697); [OPERATIONS.md](OPERATIONS.md) §“VOD continuity limits” |
| P1-9 | Refresh/timeout/lease constants in [`migration.rs`](../crates/plurx-core/src/cluster/migration.rs#L5750); sequential sampling at `migration.rs:6517`; fence readiness in [`serving_fence.rs`](../crates/plurxd/src/serving_fence.rs#L364); VOD steady/seek acceptance receipts |
| P1-10 | Post-operation baseline reset in [`scripts/playback-lab`](../scripts/playback-lab#L2745); scoring at `playback-lab:2175`; operation manifest in [`cases.json`](../tests/playback/cases.json#L64) |
| P1-11 | Historical max frontier in [`vodserve.rs`](../crates/plurxd/src/vodserve.rs#L465); driver use at `vodserve.rs:4993`; continuous protected range at `vodserve.rs:6157` and [`titlestore.rs`](../crates/plurxd/src/titlestore.rs#L55) |
| P1-12 | `Hold::NoRoom` in [`prodsched.rs`](../crates/plurxd/src/prodsched.rs#L284); VOD hold projection at `vodserve.rs:3318`; resolver at [`playback_control.rs`](../crates/plurxd/src/playback_control.rs#L1869); `a_vod_seek_past_a_hole_is_judged_by_the_media_ahead_of_the_client` |
| P1-13 | Android seek inference in [`Controller.playbackControlObservation`](../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt#L1454); mapper target in [`PlaybackControlSnapshotMapper.kt`](../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSnapshotMapper.kt#L51); web preview clear and teardown in [`index.html`](../crates/plurxd/src/web/index.html#L9593) and `index.html:9852` |
| P2-1 | New playback ID at [`Controller`](../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt#L365); actual quality in the session request at `Controller.kt:1085`; hard-coded Auto snapshot at `Controller.kt:1489` |
| P2-2 | Settings defaults in [`http/system.rs`](../crates/plurxd/src/http/system.rs#L1811); web-only Auto use at [`index.html`](../crates/plurxd/src/web/index.html#L9211); Developer copy and typeless setting at `index.html:12753`; mutable envelope caveat at [`transcode.rs`](../crates/plurxd/src/transcode.rs#L896) |
| P2-3 | Apple capability in [`Caps.swift`](../clients/apple/Sources/Caps.swift#L256); rejection test `PlaybackControlReporterTests.testUnknownActionIsAProtocolError`; web/Android false capabilities at `index.html:6668` and `PlaybackControlSnapshotMapper.kt:268` |
| P2-4 | Explicit CI exclusions in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml#L523); nightly install in [`validation-nightly.yml`](../.github/workflows/validation-nightly.yml#L18); browser lookup in [`scripts/playback-lab`](../scripts/playback-lab#L281); eight retained run IDs |
| P2-5 | Control metric families in [`playback_control.rs`](../crates/plurxd/src/playback_control.rs#L11090); post-restart zero action/preparation snapshot and node-local telemetry query |
| P2-6 | Four-node `03379035` version query, redacted startup capability logs, and fragment-index queue aggregate retained in the evidence companion |
| P2-7 | Web verdict arm/clear in [`index.html`](../crates/plurxd/src/web/index.html#L7031); Apple lifetime in [`PlaybackControlSession.swift`](../clients/apple/Sources/PlaybackControlSession.swift#L130); Android lifetime in [`PlaybackControlSession.kt`](../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSession.kt#L120) |
| P2-8 | Old-owner ask floors in [`PlaybackControlSession.swift`](../clients/apple/Sources/PlaybackControlSession.swift#L269), [`PlaybackControlSession.kt`](../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSession.kt#L195), and web `askForAction` at `index.html:6886` |
| P2-9 | Range parser in [`http/stream.rs`](../crates/plurxd/src/http/stream.rs#L2448), response mapping at `stream.rs:2480`, and parser-only tests at `stream.rs:3632` |
| P2-10 | Hard-coded wait caps in [`vodserve.rs`](../crates/plurxd/src/vodserve.rs#L114), pool refusal in [`waitpool.rs`](../crates/plurxd/src/waitpool.rs#L177), and promised configuration in [VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) §2.3 |
| P3-1 | Server validation in [`playback_control.rs`](../crates/plurxd/src/playback_control.rs#L363); Apple validation in [`PlaybackControlReporter.swift`](../clients/apple/Sources/PlaybackControlReporter.swift#L176); Android validation in [`PlaybackControlReporter.kt`](../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlReporter.kt#L195); web validation at `playback-control.js:52` |
| P3-2 | Stale comments at [`http/hls.rs`](../crates/plurxd/src/http/hls.rs#L5392); stale milestone/current-state claims in [PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md); false cutover claim in [VOD-CUTOVER.md](VOD-CUTOVER.md) |
| P3-3 | Progressive pace parser and ffmpeg argument use in [`http/stream.rs`](../crates/plurxd/src/http/stream.rs#L79) |

## Client correction review — races found after the first repair

The first client repair was not accepted on its initial review. An independent
adversarial pass followed the code through delayed responses, superseded seeks,
node failover, paused playback, and presentation callbacks. The following
matrix records every finding and the production boundary used to close it; it
is intentionally more specific than the one-row history-audit anchor, which
can associate a corrective commit with only one source/test pair.

| Priority | Adversarial finding | Implemented correction | Regression boundary |
|---|---|---|---|
| P1 | Android could retire a new seek from an outgoing or pre-render frame callback | Removed the pre-render metadata hook; each executed destination arms one post-mutation output listener carrying its exact intent sequence, and failover re-arms only an already-executed destination | `Controller.armVideoPresentation`; `capturedFrameGenerationCannotSettleItsExecutedSuccessor` |
| P1 | Repeated Android ±10-second commands collapsed onto the stale player clock | Relative commands accumulate against the newest optimistic destination before the 100 ms coalescer runs | `Controller.seekBy` / `PlaybackIntent.beginRelativeSeek`; `rapidRelativeSeeksAccumulateAgainstTheOptimisticDestination` |
| P1 | Android failover inherited the failed attempt's owner/deadline and forced paused playback to resume | Failover invalidates the old request generation, starts a fresh playback attempt, preserves the stall budget, retains the prior `playWhenReady`, and rebinds pending presentation | `Controller.retryMediaOnNextNode`; `transportFailoverInvalidatesAWaitingStallWithoutResettingItsBudget` |
| P1 | Apple/web could overshoot the 20-second control deferral by another poll or ask window | Apple sleeps to the earlier of its cadence and the absolute monotonic deadline; web skips a new ask at or beyond that boundary | `recoveryPollDelayMs` and `persistentWait`; exact-boundary timer tests on both clients |
| P1 | Apple excluded an executed-but-unpresented destination from recovery and could wait forever for presentation | Executed destinations remain stall-eligible; the presentation monitor has an eight-second absolute deadline and generation-fenced recovery | `PlayerSeekState.allowsStallRecovery` / `beginSeekPresentationMonitor`; seek-state and deadline tests |
| P1 | Apple quality, audio, and reopening subtitle replacements had no authoritative target or presentation settlement | Each command synchronously publishes an exact film target and generation before any await; in-place subtitle mutations settle explicitly, while an attached replacement must present | production `selectQuality`, `selectOriginalQuality`, `selectAudio`, and `selectSubtitle`; `testQualityAudioAndSubtitleCommandsCreateANewPresentationDestinationSynchronously` |
| P1 | Apple bitmap-overlay selection changed visible output before urgent intent publication | Overlay teardown/installation moved behind the control publication and stale-action check | `applySubtitleSelection`; production-source contract assertion |
| P1 | Apple native seek completion was not fenced to the item/open generation | The seek captures both and rejects a completion from any replaced item or newer open | native arm of `seek`; source-level generation contract plus seek-state presentation tests |
| P1 | A late terminal response could re-arm a verdict and stop reporting after viewer intent was cleared | Android, Apple, and web tag pending/retry requests with a local intent generation; storage and reporter shutdown both require the current generation | each `PlaybackControlReporter`/session; delayed-terminal tests on all three clients |
| P1 | Android Original copy bodies omitted both height and Auto intent, so the server inferred Auto | Every new Android session sends `quality_auto`; Original explicitly sends false while legacy omitted bodies retain compatibility | `subtitleSessionBody`; wire serialization test and server `CreateSession` conversion test |
| P1 | Android accepted one proximity sample and Apple required every delayed audio sample to remain within a fixed landing window | Both audio-only paths require execution and two advancing samples on a plausible target-relative timeline. The Astra follow-up below replaces the fixed window with a bounded active-time/rate allowance | both `presentedAudio` implementations and their delayed-cadence/rate/pause regressions |
| P2 | Android/Apple terminal leases used wall time | Lease age now uses monotonic uptime on both platforms | verdict-store lease tests plus production monotonic clock anchors |
| P2 | Earlier evidence mapped helper behavior rather than the user command/race seams | Tests now invoke production quality/audio commands or load shipped web functions, and this matrix names every production seam explicitly | `tests/client-fixes.toml`, `AppleClientTests`, and the shipped-source web harness |

The first correction set is committed as `78223cef`; its documentation head is
`17f6e873`. The fresh Astra pass found further failures in that set. The earlier
green focused tests do not establish that those failures are closed. Forgejo
PR #6 remains WIP until the complete new tree has exact-head approval and a
green effort gate; the full qualification stays deferred until the effort is
frozen.

### Astra follow-up — composition and evidence must survive real timing

The new pass exercises combinations rather than isolated command helpers.
In particular, a pending seek at 90 seconds followed by a quality or subtitle
change must not reopen at the outgoing element's 10-second clock. A delayed
callback from the outgoing session must not settle the replacement simply
because the track number or media timestamp happens to match.

| Priority | Failure exposed | Correction and current evidence |
|---|---|---|
| P1 | Stream-changing commands copied the outgoing observed position instead of the pending destination | All clients carry the pending target through selection changes. Android quality-then-seek composition and the web decision-await boundary remain under active correction |
| P1 | Subtitle-ready callbacks crossed reporter replacement/end, including same-track replacement | Apple and Android validate the session/intent generation again when the queued UI callback executes. Apple's focused session regressions pass; Android re-verification is in progress |
| P1 | An in-place subtitle selection falsely retired a still-pending media seek | Selection completion is distinct from destination presentation. A newer subtitle intent inherits and executes the pending destination |
| P1 | Healthy 1 Hz audio observations never entered a fixed ±250 ms first-sample window | The window accounts for monotonic active playback time and sampled playback rate, capped at eight active seconds; pause/background time does not expand it. Audio then requires a later strictly advancing sample. Focused cadence/rate/pause regressions run on all clients |
| P1 | A browser frame callback queued before execution could arrive afterward and settle it | The callback carries the execution epoch captured at registration; the detector rejects predecessor epochs. Shipped-source web regression passes |
| P1 | A decoder could freeze while the element still reported playing, with no `waiting` event | The web monitor samples clock/frame progress independently. Its adversarial review also caught `playing` cancelling a wait without real progress; that correction is active. Android is adding a separate unpresented-target deadline so a wrong-position advancing clock cannot hide the failure |
| P1 | A second web control ask could extend beyond the absolute 20-second wait budget | The ask receives the original absolute deadline and limits both its initial timer and extension to remaining time. A deterministic just-before-deadline regression passes |
| P1 | A delayed web decision replaced a newer seek or selection | The open gate previously tracked only other opens, not commands issued during the await. The replacement must capture and validate one coherent current intent; active correction |
| P1 | Internal web source resets and unconditional `play()` lost the viewer's pause intent | Desired playback must be separate from the media element's temporary paused state during reset; active correction includes pause-during-open regressions |
| P1 | Web audio sync could reopen direct playback and silently omit the correction; copy-HLS fallback left the destination unexecuted | Decision routing must include the requested offset, and every accepted source mutation must mark its exact execution. Both production paths are under correction |
| P2 | Abandoned web transcode opens retained server sessions | Stale successful opens must release only their newly created session, without touching the current owner; active correction |

Apple's latest focused run passed 31 tests; the intermediate Android run
passed 98 and the web static fast lane passed. These are development proofs,
not exact-head approval. In particular, `AVPlayerItemVideoOutput` proves that a
decoded frame is available at its returned display time, not that
`AVPlayerLayer` physically displayed it. Device qualification must test that
remaining renderer boundary; documentation and source comments must not
promote availability into compositor evidence.

The next cross-platform review found the same composition defect one layer
deeper: invalidating an old quality command's task was insufficient if the
newer seek still executed against the old recipe. Android now carries the
pending plan replacement through every later command and retains desired
play/pause state across controller reconstruction. Apple is introducing an
explicit desired-versus-attached recipe revision, restoring transport from
live viewer intent after each await, and forbidding a same-position older
open from acknowledging the newer recipe. Its target-presentation deadline
cannot transfer recovery to an ordinary clock-based deferral, because an
advancing wrong-position clock could cancel that deferral. Readiness-triggered
subtitle disable/re-enable also needs the current viewer-action epoch through
both asynchronous mutations.

Current retained development evidence is Apple commit `96065b89` with 31
focused simulator passes and Android commit `0248fa68` with 98 focused JVM
passes. Subsequent review found that Android's pending-quality state still
did not retain an unattached audio/burn/offset recipe, and that its target
retry budget belonged to the replaceable controller. Commit `c5504641` now
retains the complete requested/attached recipe, waits for confirmed native
track selection, and transfers the single retry owner across controller
replacement. Its 105 focused JVM regressions pass; exact-head independent
approval remains pending.

Web's complete static fast lane passed after coherent full-open intent,
transport intent, abandoned-session cleanup, and silent-freeze corrections.
The next review found that attachment callbacks were incorrectly fenced by
seek sequence: a newer native seek before `MANIFEST_PARSED` suppressed startup,
while late direct metadata could restore an old start position. Attachment
identity is now separate from command identity, metadata applies the newest
target, and departed callbacks cannot mutate the replacement. The production
HLS/direct attachment regressions and the static fast lane pass. None of these
intermediate results is an exact-head release approval.

The independent web pass then rejected partial audio/burn/Auto/decoder-rescue
opens that bypass the coherent full-open owner: a later VOD seek could cancel
the replacement and keep seeking an old or detached recipe. It also reproduced
a native `seeking` event cancelling the watchdog's wait without resetting its
fired latch, and found that `load()` discards queued media events while the
application retains their numeric expected-event counters. A departed play
promise can likewise decrement a replacement's counter. These are active
corrections with real asynchronous command and media-task-queue regressions,
not closed findings merely because the prior static suite passed.

Apple's independent pass rejected unfenced open/failover subtitle mutations,
legacy-burn fallback without a dirty recipe, stale pause intent on controller
reuse, title decisions crossing stop/start, and offline seeks entering the
network recipe gate. The follow-up adds explicit lifecycle/operation ownership
and suspension-boundary regressions. It is not approved yet.

### VOD demand slice — scoped adversarial result

Commit `abb872ba` contains the demand/scheduler corrections. The independent
reviewer repeated all 103 focused tests: 66 VOD tests, including real FFmpeg
generation; 11 wait-pool tests; and 26 scheduler tests. Pinned Rust 1.97.1
compilation, all-target Clippy with denied warnings, formatting, and diff checks
passed on the reviewed source. Its reviewed diff digest is
`38a87d666049d3352951193764e8fcbc7fc6e053ba8cf101ecd58bf422fbc505`.

The review required corrections beyond the initial patch:

- admission and materialization-clock binding commit atomically; refused GETs
  own neither a clock nor a sleeping task, and cancellation retires both;
- accepted control sequence and demand anchor have no cancellable gap;
  acceptance disables old marker speculation before asynchronous rebuilding;
- a new control epoch may restart its sequence, and legacy viewers still
  compete with controlled viewers for foreground work;
- scheduler choice skips already-materialized obligations, including the
  publication-before-notification window;
- a notified request keeps its file pin through the actual response file open;
- a zero-byte eviction sweep stops the producer instead of immediately
  self-waking, and queued writes retain their producer epoch fence;
- the file-open recheck spends the original HTTP deadline rather than starting
  another full wait budget afterward.

Current-main integration changes the control result shape and test request
fixtures, so this pre-integration approval is not approval of the merged tree.
Fresh integrated compilation, focused tests, and exact-head review are required.
Session-detach cleanup, total serving admission, typed scoped timeout/NoRoom
actions, disjoint-viewer capacity, and transition transactions remain separate
open architecture work.

### Astra continuation review — keep transport, media, and title ownership distinct

Apple correction `856d9a0a` closes the six continuation classes from the
follow-up review. Decision success and failure belong to the title lifecycle;
native subtitle/audio preparation must recheck the exact command before
mutating AVFoundation; fallback flags change the desired recipe; offline
attachment stamps its recipe and seeks locally; a restarted title restores
play intent; and a readiness wait must prove ownership **before** seeking the
shared player. Pause can retain a resume destination without authorizing
playback. The exact three-file manifest was independently approved, with 43
focused simulator tests including 12 injected asynchronous controller
regressions, and generic iOS/tvOS compilation. This is not physical playback
qualification.

The next Android review found four further classes in the production wiring,
despite passing pure recipe tests:

- Pause invalidated the same request token used by an admitted audio/burn/
  offset create. Its response was released, and Resume did not attach the
  retained recipe. Transport observation authority must be separate from the
  media request authority.
- A stale normal or 400-fallback stall response was filtered to `null` before
  the controller could release its newly created session. Every successful
  resource response needs either exact attachment ownership or exact cleanup.
- A departing item's error could start failover of its old URL and revoke a
  newer desired recipe. The pending replacement owns that interval, including
  a same-recipe seek create.
- A stall monitor awaiting control could miss Pause→Resume, a local subtitle
  change, or background→foreground entirely. Rejecting the old response was
  correct, but its `fired` latch remained set forever. Invalidation must also
  synchronously relinquish that observation; numeric recovery budgets must
  not reset merely because playback was paused.

Android correction `310a226b` passed its effort commit gate, 123 focused JVM
tests, and the 115 validation-contract tests; its four-file source manifest
was independently approved. Exact PR-head review still follows integration.
Tests now combine
the production request coordinator, attachment boundary, transport guard,
and stall tracker with suspended server/control responses, rather than
testing recipe equality alone.

Web review extended the same principle to media-element task queues, partial
opens, and title transitions. Old load/play rejection callbacks must not
consume a new attachment's native play/pause tokens. A native `seeking` event
must not cancel a fired recovery deadline without a new viewer intent or real
progress. Diagnostics and next-episode lookups must be bounded and fenced to
the exact title. A pending audio/burn/Auto/fallback recipe must survive a
newer seek. Finally, a decision or create whose response headers or JSON body
never arrives needs an absolute preparation deadline; advancing predecessor
frames do not prove that the requested replacement is progressing.

Web correction `0a70d6fb` passed its tracked effort commit hook, the full static fast lane, and independent
source review at ordered three-file SHA-256 manifest
`8d6eff271d2c3e5b96990b7630f3a049c28155ff1d819e9f4a1410f3becebdc9`
(`index.html`, `web-control.test.js`, `web-policy.test.js`). One 20-second
preparation owner bounds decision, create, and body reading; supersession
aborts transport and a stale successful response releases only its own
session. The actual predecessor remains attached until successful replacement,
including failure and Retry paths. Cold Close is null-safe; Close while title
B is preparing attributes final progress only to actual title A.

The last adversarial pass reproduced retained-predecessor events stealing
the incoming owner's deadline or attributing A's playback to B. One shared
actual-attachment eligibility predicate now governs native playing/waiting,
watchdogs, progress, markers, TTFF, health/ABR, reporter observations, subtitles,
and HLS error/XHR/fragment/level callbacks. Actual shipped-source regressions
hold a partial replacement open and fire the old HLS callback set. This is
scoped ownership proof, not a physical continuity receipt or completion of
all cold pre-decision quality/track/seek inputs. Exact committed and integrated
PR-head approval remains required.

### Atomic control captures — payload and local authority travel together

Commit `fc544fad` corrects the split observation/intent audit lead with one
immutable source capture across Apple, Android, and web reporters. The envelope
retains the snapshot, desired generation, and local lifecycle/attachment owner
through enqueue, urgent coalescing, exact-sequence retry, and exchange callbacks.
Native source revisions reject reversed async enqueue order, including older
periodic observations of the same intent. Cadence reads the published capture,
not AVPlayer or Media3 from a reporter executor.

Adversarial review added deterministic regressions for old attachment reporters
borrowing a new attachment's capture, a cleared source slot during `409` reset,
stale subtitle-ready callbacks consuming the new intent's retry edge, and a
terminal response stopping reporting after a newer same-attachment intent.
Fresh admission requires the reporter's bound local owner at enqueue and send;
already-started retries keep their original envelope. Terminal-only suspension
can yield to newer intent, while explicit End and protocol failure stay final.
The regression bridge is retained separately in `7d5f959a`.

The frozen source passed scoped independent review. After integrating foundation
task [#19](http://192.168.4.7:3000/noirr/plurx/pulls/19) at `12be7b5b`, the exact
tree again passed 103 Apple tests, 102 Android tests, tvOS compilation, and
complete web checks. Exact integrated-head approval and the development gate
remain required before merge. These captures do not implement cold
desired-input ownership on every platform, executable prepared handoff, or
physical continuity/performance qualification.

### Apple initial-decision ownership — desired input precedes metadata

The next Apple slice retains one initial decision request before the first
network await: explicit audio, subtitle Default/Off/track, quality Auto/Original/
height, and the exact non-menu height when supplied. A cold recipe command
replaces that request; its own generation rejects old success and error even
when the viewer chooses A → B → A on the same title. Defaults from a delayed
decision cannot overwrite the newer request. Seek and Pause remain independent
of decision ownership: neither forces a repeated decision nor grants an old
operation permission to resume playback.

An absolute 20-second initial-decision deadline exposes the existing Try Again
surface without discarding the recipe, destination, or Pause. This is a bound on
the decision request, **not** a claim that the entire create/attach/preparation
pipeline has one deadline. That broader owner and make-before-break transaction
remain required.

The independent adversary found three additional boundaries in this slice:

- a successful decision followed by a failed first create crossed into the
  warm Retry arm and implicitly requested Play; Retry now authorizes another
  preparation without changing transport, and repeated no-item failures retain
  the resume destination;
- a control-sequence await could resume after a new title started and submit
  an old body using the new mutable file ID; creates capture file/lifecycle and
  revalidate after the await, including before the bounded 400 fallback;
- a queued periodic observer, or one firing with no attached item, could replace
  a 40-second resume position with zero; the actual callback now requires its
  current title, an attached item, and no active preparation. Delayed black-frame
  recovery also retains item/action ownership.

Commit `3155bfe9` retains these corrections. The 19 focused
`PlayerOperationOwnershipTests` exercise actual controller
commands, captured decision requests, produced session bodies, repeated create
failures, cancellation-ignoring decisions, injected deadline/read boundaries,
and the actual periodic callback. The expanded 50-test control/presentation
regression run and iOS/tvOS compilation pass on the same reviewed source.
The session-body fixtures deliberately stop before network attachment; they do
not establish physical decoder, audio, or display continuity. Existing offline
and native-selection ownership tests remain in the focused run.

### Executable handoff audit — durable settlement is not a running successor

The initial audit used Forgejo main `4ce33f95` and concurrent
[PR #5](http://192.168.4.7:3000/noirr/plurx/pulls/5) at `20951a9`. It was
rechecked after that PR merged into main `15f88e53`: durable,
cancellation-independent acknowledgement settlement and exact predecessor
compare-and-swap are implemented, but `stage_prepared_successor` still writes
`encoder: staged` without starting a worker. The effort's local integration
preserves this ledger and its own accepted-demand publication; it does not
create a second transaction. The anchors below identify functions rather than
line numbers that move as the branches merge.

| Boundary | Observed implementation | Required completion |
|---|---|---|
| Candidate creation | `http/hls.rs::stage_prepared_successor` persists a synthetic `StartResponse` with `encoder: staged`; no media worker starts | Reserve capacity, create an exact immutable recipe, start its worker, and retain its real timeline/codec metadata |
| Priming reads | `media_sessions.rs::classify_durable_route` rejects the candidate publication sentinel; settlement does not clear it | Grant only the exact prepared candidate bounded priming-read authority while the predecessor remains active; never weaken the normal publication barrier globally |
| Client readiness | Preparation acknowledgements can arrive without an implemented standby player or proven media runway | Production clients must prepare a real standby pipeline and report the owned media configuration, contiguous runway, and target timestamp |
| Commit | PR #5 commits on client `Committed`; its usable-route test checks pointers and parseable metadata, not media GETs | Server authorization must follow owned readiness and be durably replayable after a lost response |
| Switch and drain | No production client executes the complete transaction; Store commit immediately retires the predecessor and cache pins | Confirm the exact switch, then drain bounded predecessor reads/output without restoring it as active authority |

The desired transaction is:

```text
accepted desired intent → real candidate → prime → client ready
                        → durable server commit → client switch → bounded drain
```

The following gaps are part of the remaining implementation, not optional
documentation debt:

1. **Canonical desired intent.** `PreparationCandidateInputs` has a selection
   and route but no normalized recipe digest, relevant desired revision, or
   destination revision. A delayed B can stage after C or after a same-quality
   seek without the predecessor pointer changing. Capture the desired token
   synchronously at accepted control and check it at worker creation,
   publication, readiness, commit, and switch. Ordinary heartbeat sequence
   numbers must not invalidate unchanged intent.
   Also test overlapping server creates: A and B can both capture predecessor
   P before placement; A activates, then the newer B loses predecessor CAS.
   Client-side stale-response fencing does not make B the server's final
   activation. HTTP cancellation does not prove a detached activation owner
   stopped. The canonical desired owner must serialize or safely rebase this
   path; exercise it with advisory control disabled as well as enabled.
2. **Safe film-time mapping.** `stage_prepared_successor` derives resume from
   `media_origin_ms + fetched_through_ms`. That can skip a client's buffered
   but unseen film. Use the explicit seek destination, or a bounded aligned
   cutover from the owned playhead and intersecting old/new runway. Persist
   the actual worker origin. The required regression has playhead 30 seconds
   and fetched frontier 150 seconds: quality change must not jump to 150.
3. **Executable VOD recipes.** `vodserve::try_create_with_release_fence`
   refuses transcode and burn; `Recipe`, `spawn_generation`, and `vodgen`
   still rely on copy-specific compressed-fragment landing. `segplan` has a
   transcode grid planner, not a complete VOD encoder. Add copy/transcode
   strategies under the existing immutable manifest and demand model;
   deterministic keyframes, timestamps, initialization identity, audio,
   grade, and burn settings belong in rendition identity. Real FFmpeg tests
   must generate and decode transcode/burn output across 0→90→3 seeks and
   producer restarts.
4. **Original is not source-height Manual.** The effort already added an
   explicit `QualitySelection::Original` wire value before `ad79ae81`;
   the earlier statement that this enum was absent is superseded. The
   remaining defect is in the producer resolver: `candidate_request` still
   keeps an existing transcode in transcode mode for every quality value.
   Finish Original semantics there rather than add another wire enum. Audio
   conversion need not re-encode video; burn and incompatible HDR requirements
   must be reported honestly. Test Original→720p→Original, including
   source-height Manual as a distinct case.
5. **Truthful capacity and delivery.** `DeliveryView::from_status` drops VOD
   producer decisions and delivered throughput, while
   `PreparationConditions::headroom_refusal` requires measured headroom.
   Count completed delivery separately from abandoned GETs, contiguous
   runway separately from scattered cached segments, and encoder/cache
   admission separately from network rate. Unknown is not unlimited
   capacity. Apple and Android already provide observed-rate inputs in
   current main; the missing VOD delivered metric is server-side.
6. **Production client execution.** Apple, Android, and web reporter
   vocabularies support advisory hold/retry/terminal, not the complete
   transaction. Apple's `dualPlayerPreparation: true` device capability is
   not evidence that a standby transaction executes. Advertise only
   implemented, currently admissible paths and expose operational
   prerequisites in the requested Developer Enable section.

The implementation order is: consume PR #5's durable settlement; establish
the intent/recipe contract and truthful VOD status; complete real VOD recipes;
create and prime real staged workers; extend commit/switch/drain semantics;
integrate the three client adapters; then qualify failure injection and
physical continuity. Every stage needs lost-response, supersession, and
deadline tests. Final media tests also cover node loss before/after commit,
exhausted capacity, long prebuffer, decoder rejection, pause, offline
exclusion, and rapid A/B/C changes. Successful acknowledgements alone are not
the acceptance oracle: presented film timestamps and audible gaps are.

## Target architecture — immutable renditions plus one replacement transaction

The target has one media model and two transition mechanisms:

```text
                         +------------------------------+
client intent ---------->| sequenced playback controller|
render + buffer evidence | owner + action + deadline CAS|
                         +---------------+--------------+
                                         |
                     same timeline       | recipe / owner change
                     same item           v
                +----------------+   RESERVE -> PRIME -> READY
seek ---------->| immutable VOD  |                 |       |
                | rendition set  |                 + ACK --+
                +-------+--------+                         |
                        |                                  v
                  aligned segments              COMMIT AUTHORITY
                        |                                  |
                        +--------------------------> switch boundary
                                                           |
                                                retire predecessor
```

### Media plane

1. One immutable title timeline, addressed in film time.
2. One rendition identity per complete recipe: source version, video/audio
   recipe, grade, subtitle mode/revision, segment geometry, and encoder
   compatibility revision.
3. Copy, transcode, and burn renditions implement the same `init + segment N`
   contract. A segment may be materialized lazily, but its time range and bytes
   never change after publication.
4. Demand is reference-counted and generation-fenced. Disconnect or
   supersession cannot leave a poison watchdog behind.
5. Quality adaptation has exactly one owner. The first implementation is the
   sequenced control transaction below, because it can prove admission and
   media readiness before telling a player to move. A later multivariant
   optimization is allowed only under one of two explicit contracts:
   client-owned adaptation where every advertised variant already meets the
   immediate segment-availability SLO and reports the actual selected rung, or
   server-owned adaptation where only a fully primed candidate is exposed and
   selected. An unprimed lazy rendition is never advertised to native ABR.

This ruling changes the earlier draft recommendation. Native players may ask
for any advertised variant immediately; they do not participate in the
prepare/ready/CAS protocol merely because the playlist is multivariant. Lazy
start without a readiness contract would move the stall from player
replacement to the first segment of the new rung. Keeping every rung hot would
violate the one-encoder steady state. Prepared handoff is therefore the
correctness path for quality as well as codec/grade, with multivariant HLS a
later optimization after availability and authority are proved.

### Control plane

Prepared replacement owns every recipe change initially: quality, codec/grade,
burn recipe, audio topology, and node ownership.

| Phase | Authority | Required invariant |
|---|---|---|
| Propose | controller actor | newest accepted selection and render evidence only |
| Reserve | admission | predecessor remains authoritative; bounded overlap is explicit |
| Prime | media owner | real init plus measured platform runway, not a placeholder row |
| Prepare | action response | action names successor, film boundary, recipe, generation, and deadline |
| Ready | client acknowledgement | metadata and buffered film time are monotonic and action-fenced |
| Commit | one durable CAS | predecessor, action, sequence, selection digest, and deadline all match |
| Switch | client | old player remains until new player can render the boundary |
| Retire | media owner | stop production after commit; retain published bytes for pinned readers |
| Abort | either side | destroy successor only; current stream remains playable |

### Recovery authority

Producer state may advise how to fill future buffer; it may not suppress a
finite renderer recovery. Every platform needs one monotonic no-progress clock
with these terminal outcomes:

- progress: reset the clock and recovery budget;
- transient producer delay: wait only inside the remaining finite deadline;
- fetch wedge: reconnect identical delivery once;
- decoder/item wedge: reattach or use a compatible decode fallback once;
- retryable producer failure: follow server pacing inside a bounded budget;
- permanent failure or exhausted budget: show one actionable terminal error.

No branch returns to the same frozen state without advancing a deadline,
spending a token, or changing an observable condition.

## Repair sequence — reviewable slices on one effort

| Order | PR outcome | Findings closed | Proof before merge |
|---:|---|---|---|
| 1 | Review, status, and acceptance contract | P2-2, scope for all | docs/static contracts + adversarial approval |
| 2 | Truthful playback harness and nightly browser | P1-10, P2-4 | transition scoring + quality operations; requested-case proof; nightly resolves installed Chromium |
| 3 | Finite client stall recovery, playback identity, and seek intent | P0-1, P0-2, P1-13, P2-1, P2-7, P2-8 | web + Apple + Android units; source-level player contract; focused builds |
| 4 | Serving-authority liveness | P1-9 | delayed-observer/term-change tests; immutable-read drain; focused cluster gate |
| 5 | VOD demand generations, fairness, and failure actions | P1-1, P1-2, P1-11, P1-12, P2-10 | HTTP seek reversal/storm/disconnect tests; typed failure action |
| 6 | Real VOD transcode/burn rendition | P0-3, P1-8 foundation | copy/transcode/burn lifecycle; far seek; production-default configuration |
| 7 | Prepared quality replacement | P0-4 quality slice, P1-3 through P1-7 | cold-candidate/admission/oscillation matrix; 20 uninterrupted down/up changes per platform |
| 8 | Remaining prepared replacement axes | P0-4, P1-4 through P1-8, P2-3 | stale/deadline/race matrix plus codec/grade/burn/audio/owner handoff tests |
| 9 | Developer enablement and durable rollout evidence | P2-2, P2-5, P2-6 | settings/API/UI tests; restart-persistent action ledger; readiness refusal |
| 10 | Cluster VOD continuity and input hardening | P1-8, P2-9, P3-1, P3-3 | node-loss drill; shared conformance vectors; HTTP range/input tests |
| 11 | Acceptance restoration and final qualification | all | HTTP VOD matrix, browser matrix, native device evidence, unit suite, promotion gate |

PR boundaries may move when compilation exposes a tighter dependency, but no
PR may claim a finding without the user-visible regression for that finding.
Each task targets `effort/streaming-reliability`, uses the effort fast lane,
gets an exact-head adversarial review, and incorporates its findings. The full
unit suite and main-promotion qualification run once after every fix is on the
frozen effort candidate.

## Acceptance — what “works well” means

### Playback SLOs

| Operation | Required result |
|---|---|
| Cold copy/remux play | first decoded frame p95 ≤ 2.0 s, max ≤ 3.0 s on LAN; zero stalls, reopens, or fatal media errors |
| Cold transcode/burn play | immutable VOD; first frame p95 ≤ 5.0 s, max ≤ 8.0 s on a qualified hardware path; an unqualified node refuses placement |
| 20 then 100 distant seeks | same playback/session where recipe is unchanged; final target first frame p95 ≤ 1.0 s, max ≤ 2.0 s; landing error ≤ 250 ms; zero poisoned renditions |
| Manual same-codec quality change | 20 down and 20 up per platform; zero stall/reopen; film-position error ≤ 250 ms; video frame gap p95 ≤ 100 ms and max ≤ 250 ms; audio gap max ≤ 100 ms; runway never below 2.0 s |
| Auto 8 → 1.5 Mb/s cliff | one downgrade begins within 10 s and reaches a sustainable rung; zero stall/reopen; runway remains above 1.0 s; film-position error ≤ 250 ms; video frame gap p95 ≤ 100 ms and max ≤ 250 ms; audio gap max ≤ 100 ms; no second move for 60 s |
| Codec/grade/burn change | every supported and qualified tuple commits successfully 20/20; predecessor survives every pre-commit failure; position error ≤ 250 ms; explicitly unsupported or admission-constrained tuples are a separate matrix and refuse before changing playback |
| Open-ended network freeze | urgent recovery starts by 12 s after established playback (30 s before first establishment); one recovery or terminal result by 45 s |
| Frozen decoder with buffered media | recovery starts by 12 s; advisory hold cannot defer it past 20 s; one recovery or terminal result by 45 s |
| Owner loss | already-playing immutable media continues and a prepared owner commits before reported runway is exhausted; terminal is an interim limitation, not acceptance |
| Pause/background/resume | first decoded frame ≤ 1.5 s after foreground play intent; position error ≤ 250 ms; no duplicate producer |
| Two-hour 4K/NAS soak | media-clock ratio 0.98–1.02; zero stalls, reopens, fatals, or frame gaps > 250 ms; dropped-video-frame rate < 0.1% |

The lab measures from the monotonic sample immediately before the operation,
not from a reset after it. “First frame” is the first newly decoded/presented
video frame at the target generation; video gaps use consecutive presented-
frame timestamps, audio gaps use the platform audio-render timestamp, position
error compares reported film time at commit with the first presented successor
frame, and runway is the contiguous buffered interval ahead of that film time.
Twenty-run p95 uses nearest-rank sample 19; the max bound still applies to every
run. A skipped case, missing browser/device, absent timestamp, or operation that
never entered playback is a failed evidence run, not a pass.

Until the nightly Auto cliff has a retained multi-run rollup, its one observed
transition must individually stay at or below 100 ms. This is deliberately
stricter than applying only the 250 ms absolute ceiling and prevents a visible
200 ms hitch from passing while the final p95 qualification remains pending.

### Failure and race matrix

Tests must inject: rapid A → B → C selection, seek during prime, pause during
prime, client disconnect at every acknowledgement, expired commit, predecessor
owner change, successor owner loss, admission refusal, disk full, index absent,
producer crash, partial segment write, stalled storage read, late predecessor
control, duplicate acknowledgement, and response loss after durable commit.

For every pre-commit failure, the assertion begins with “the predecessor is
still current and playable.” For every post-commit failure, recovery begins
from the committed successor and never resurrects its predecessor as current.

### Evidence hierarchy

1. pure policy tests prove deterministic selection and fencing;
2. server integration tests prove storage, process, timeout, and HTTP effects;
3. real-browser tests prove the shipped JavaScript and media stack;
4. Apple/Android builds and device tests prove native lifecycle behavior;
5. a two-hour 4K run and forced fault run prove the operational claim;
6. post-deploy telemetry proves the fleet is behaving under real use.

A lower layer cannot substitute for a higher one. In particular, action JSON
round-tripping does not prove an action is reachable, and a unit-tested timer
does not prove the installed player invokes it.

## Operations — safe enablement is a visible contract

Settings → Developer will contain one **Enable streaming controller** section.
It will not conceal incomplete code behind a build flag. Before activation it
must show:

- all serving nodes on one compatible build and a stable cluster owner view;
- control protocol/action versions supported by each installed client;
- fragment-index coverage and queue health for the selected libraries;
- per-node copy/transcode/grade/burn capability and scratch headroom;
- last passing browser, Apple, Android, seek-storm, and long-run evidence;
- known unsupported transitions and the exact fallback/refusal behavior;
- whether activation affects new sessions only and how to disable safely.

The section inventories and disposes every existing playback gate:

- `playback.control_protocol_v1`: enable reporting/advisory actions only when
  the installed client vocabulary is compatible; enable prepared replacement
  only when that executable action version and the readiness matrix pass;
- `playback.auto_abr`: show that it is currently web-only; fold it into the
  controller's adaptation owner and remove the separate preview switch after
  cross-client qualification;
- `playback.hls_typeless_sliding`: keep only while growing live recovery
  exists, qualify its prune-boundary behavior, then remove it with that engine;
- `playback.vod_live_recovery`: remove the Cargo feature gate, move the runtime
  emergency control out of ordinary Playback, default it off, and delete it
  when immutable transcode/burn coverage lands.

The enable control must refuse when a hard prerequisite is false and explain
which one. Warnings may be acknowledged only for evidence debt that does not
make the executable contract false. Enabling advisory control before prepared
replacement is complete may be supported as an explicitly narrower mode; it
must not advertise `prepare_replacement` or promise transparent changes.

## Fleet snapshot — useful evidence, not a release certificate

At the audit snapshot all four nodes ran
`plurxd 0.3.0 (v0.3.0-639-g03379035)`. Containers were healthy after a
coordinated 2026-09-04 restart. `nynuc`, `nuc3`, and `nuc4` validated QSV and
VAAPI for basic work, with node-specific Main10/HDR failures; NVENC could not
load CUDA. `nynuc` showed a degraded fragment-index queue. Seven-day playback
telemetry on the active nodes contained real stalls and recoveries, including
the long held Apple freeze described in P0-2.

The post-restart sample contained only producer-pass events and no new stall
record at the time queried. That is too little traffic and time to say the
deployed build fixed the historical failures. Likewise, zero in-memory control
counters after restart prove neither rollout nor correctness.

## Decisions made during the review

1. **Keep immutable VOD; finish its recipe coverage.** Returning to mutable
   live playlists would trade known architectural bugs for old ones.
2. **Use the prepared transaction for ordinary quality changes first.** Native
   variants become an optimization only after one adaptation owner and
   immediate advertised-rendition availability are proved.
3. **Treat fetched media as delivery evidence, never presentation position.**
   The client playhead/settled seek chooses film time.
4. **A server hold is advisory to renderer recovery.** It can conserve future
   production but cannot authorize an infinite frozen picture.
5. **Make enablement honest and singular.** One Developer surface names
   prerequisites and behavior; no compile-time feature gates or undocumented
   shadow switches.
6. **Prefer explicit refusal to destructive fallback.** Until a transition is
   safely preparable, keep the working stream and explain why the requested
   change cannot be applied.

## Questions saved for later

These do not block the repair sequence. The implementation will use the stated
default unless changed later.

| Question | Default used now |
|---|---|
| Maximum brief encoder overlap for a variant or prepared switch? | Admit one successor for the measured prime window; refuse rather than evict another viewer |
| May a mode/codec change show a display reconfiguration blink? | Yes, if position and audio continuity hold; report it honestly as a mode transition |
| How much physical evidence is required before broad enablement? | Web Chrome + Safari, iPhone + Apple TV, Android phone + Google TV, then a two-hour 4K run |
| Should owner-loss recovery prefer local rebuild or peer hydration? | Prefer a ready compatible peer; otherwise rebuild on the least-loaded eligible node |
| Should unsupported burn/transcode VOD fall back to direct/remux? | Only when the decision engine proves the exact client can decode it; otherwise refuse without killing current playback |

## Completion rule

This review is complete when every accepted finding, at every priority, is
fixed and proved at its user-visible boundary or explicitly reclassified or
deferred with new evidence, a named owner, and a written rationale; every
repair PR has an incorporated adversarial review; the frozen effort includes
current `main`; the unit suite and one full promotion qualification are green
on that exact tree; and post-merge status points to the qualification receipt
and remaining physical evidence without overstating it.
