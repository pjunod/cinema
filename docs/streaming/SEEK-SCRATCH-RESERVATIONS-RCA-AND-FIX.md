# Seek scratch reservations — why repeated seeks refuse playback

**Status:** third review approved; incident repair ready to build, R3/R4
implementation gates recorded, no runtime fix implemented ·
**Written / revised:** 2026-09-20 ·
**Evidence base:** `a1414368400720884599732e3f8f3c71a9272edc`

Companion to the
[MKV duration and sliding-HLS proposal](MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md)
and [playback reference](../PLAYBACK.md). This document explains the September
20 Safari incident on media1 and proposes a bounded repair. It specifically
answers whether repeatedly pressing **Forward 10 seconds** can exhaust
playback capacity. Review the ownership and accounting transitions before
implementation; changing only the HTTP status or increasing the cap does not
resolve the incident.

The [implementation handoff](SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md)
turns this approved proposal into ordered work, file ownership, tests and
receipts for Opus. It preserves one combined promotion.

## 1. Finding — retired streams retain a producer's full reservation

A rolling-HLS seek opens a replacement stream. The old stream stops producing,
but its advertised media remains available for a finite grace period so
outstanding client requests can finish. Keeping those objects is intentional.

The regression is charging every retired stream the full **2 GiB + 64 MiB**
producer reservation throughout that period. With the default **8 GiB**
scratch cap, three such reservations fit and a fourth does not. One viewer
can therefore reproduce a capacity refusal through successive seeks even
when the files occupy much less space and the filesystem has ample room.

The failure has three parts, all required in this repair:

1. **A stopped producer's unused reservation remains charged.** Admission
   uses `max(actual_bytes, reservation_bytes)` for retired sessions too.
2. **The refusal becomes HTTP 500.** The scratch error is an ordinary string;
   it lacks the capacity classification that the HTTP handler maps to 503.
3. **A refused destination contaminates the incumbent's control state.**
   The browser keeps reporting `seeking` for a replacement that never
   attached. In this incident the incumbent's startup actor never established
   presentation and reaped it with `startup_expired`, despite the browser
   having shown frames. Keeping its media element attached was insufficient.

The proposed repair converts a retired producer's reservation to a charge for
the bytes that still exist, after every writer has stopped, and separates a
desired replacement from the attached stream's control observations. Review
also confirmed duplicate pending creates, a non-atomic global sum, and
post-cleanup charges misclassified as provisional work; those are included.

The first draft understated the terminal failure: it was **loss of the
incumbent picture**, not merely a refused seek. Correcting accounting and
changing 500 to 503 without repairing control ownership is incomplete.

**Repeated seeks remain real work.** This repair removes a false limit of
roughly three outstanding generations. It cannot promise unlimited seeks
under finite storage: enough real retained media can still fill the budget.
That condition must preserve a healthy incumbent and produce a specific
temporary-capacity refusal rather than an internal error. Full object grace
and full-ceiling reservation sizing are policy decisions R3/R4 (§7), not
assumed constraints that silently rule out 4K or multi-viewer acceptance.

## 2. Evidence — the incident contains three generations of one movie

### 2.1 Observed sequence on media1

The deployed binary identified itself as `v0.3.0-2949-ga14143684`. The incident
was file `6341`; tests must use generated fixtures, not that catalogue ID or
the user's media. Session identities below are neutral labels for the three
distinct session IDs in the logs. Times in this table are UTC, September 20.

| Time | Observation | Meaning |
|---|---|---|
| 15:28:50.518 | `vod_index_pending`; temporary live-HLS recovery selected | This playback used rolling HLS rather than immutable VOD. The pending index explains the route, not the reservation defect. |
| 15:28:51.172 | A started at source offset 0 | First producer reservation. |
| 15:28:59.131 | A's `holding transcode producer` line reported `ahead_bytes=82124595`, `global_live_bytes=2214592512`, `max_bytes=2147483648` | The global value already included a 2.0625 GiB reservation. Ahead bytes alone are not the complete inventory. |
| 15:30:34.493 | B started at 108.566 seconds | A seek admitted a successor. |
| 15:30:34.892 | A retirement settled | A stopped producing; its object promise and reservation survived. |
| 15:30:44.877 | C started at 173.974 seconds | Another seek admitted a successor. |
| 15:30:45.315 | B retirement settled | Two predecessors were now retired. |
| 15:30:45.653 | C's first playlist request validated its init | The observable startup anchor: adding the 30 s budget gives 15:31:15.653, followed by the reap about 470 ms later. |
| 15:30:47.062 | Safari logged `HLS startup presented after 1252ms` for C / a3 | The browser was showing media; this is not proof that the server actor had reached `Presented`. |
| 15:30:49.184 | a4 published a new destination | The next seek arrived about two seconds after C first presented. |
| 15:30:51.198 | a4 refused with 6,643,777,536 charged; HTTP 500 | First refusal, only about seven seconds after C started. |
| 15:30:54.215 | a5 began; a6 superseded it | Review traced cancellation before a5 reached create; do not count it as a fifth server refusal. |
| 15:30:58.148 | a6 refused with the same charge; HTTP 500 | Second refusal. |
| 15:31:12.345 | a7 refused with the same charge; HTTP 500 | Third refusal; A/B retired and C remained charged. |
| 15:31:16.123 | C reaped, `requested_reason="retired_recovery"`, `winning_reason=startup_expired`, `idle_seconds=4` | Terminal loss of the incumbent, about 30 seconds after initial serving. |
| 15:31:25.016 | Cold create refused with the same charge; HTTP 500 | Fourth refusal, followed by `stopped … owner_stopped`. |
| 15:32:51.497 | Stopped surface cleared by the user | Last recorded user dismissal in this incident. |

The refusal was:

```text
rolling scratch capacity unavailable:
6643777536 bytes charged, 2214592512 requested, 8589934592 configured
```

The arithmetic is exact:

```text
R = 2 GiB + 64 MiB = 2,214,592,512 bytes
3R                 = 6,643,777,536 bytes = 6.1875 GiB
3R + R             = 8,858,370,048 bytes = 8.25 GiB
configured cap     = 8,589,934,592 bytes = 8 GiB
excess             =   268,435,456 bytes = 256 MiB
```

Subsequent read-only inspection found approximately 72 GB available on the
backing filesystem, no remaining FFmpeg process in the container, and only
2.2 MB under the transcode directory. This establishes that filesystem
exhaustion was not the explanation. It does **not** measure each generation's
physical bytes at the instant of refusal, prove the in-memory charge later
returned to zero, or constitute a successful playback retest.

No production settings were changed, no sessions were manually deleted, and
the service was not restarted during this investigation.

**Why startup expired:** `beginPlaybackControlSeek` publishes the desired
target before create. On refusal, `executePlaybackMediaChange` retains
`pendingMediaChange` and leaves `controlSeek` set. `playbackControlSnapshot`
therefore reports the incumbent as `seeking` with that target. While its
startup state is `AwaitingPresentation`, each such exchange clears the actor's
presentation baseline. The actor requires successive accepted `Rendering`
observations of the same generation/attempt with at least 250 ms of **media
position advancement**; 250 ms is not an elapsed-time-between-requests test.
Its startup budget remains anchored to first serving and expires after 30 s.

The logged serving anchor plus 30 s matches the reap within about 470 ms,
consistent with deadline expiry followed by the next cleanup evaluation. This
is stronger evidence than merely observing that both happened in the same
minute; it does not imply the general 15 s repair interval is a 470 ms bound.
Expiry is also evaluated on actor commands, including control and media
requests, and the actor has its own deadline handling. The cleanup log records
`last_request="media-segment"` and `requested_reason="retired_recovery"`.
That field is the last renewal kind, not the event that first claimed expiry;
neither it nor the sub-second gap proves a particular expiry trigger. The
reap log itself is emitted by `reap_loop`.
The logs establish browser presentation followed by actor startup expiry;
the source establishes how the retained foreign target prevents startup
proof. The exact sequence of accepted control snapshots was not captured in
the incident excerpt. A regression must reproduce that causal sequence.
A replacement still pending for more than 30 s can trigger the same problem
without any refusal, so clearing the target only in the catch block is too
late as a general fix.

### 2.2 The change arrived with the September 19 sliding-HLS work

Times here are EDT, UTC−04:00.

| Commit / event | Time | Relevant change |
|---|---|---|
| `f78a03443` — `feat(streaming): preserve rolling object promises` | September 19, 19:39:10 | Added retained presentation ownership and grace cleanup. At this point retired accounting summed actual `live_bytes`. |
| `626ba0e41` — `fix(streaming): address final review findings` | September 19, 20:20:57 | Added full rolling reservations and charged retired entries with `max(actual, reservation)`. The reservation's drop depended on the final scratch owner disappearing. |
| `0cad9b86b`, PR #377 | September 19, 22:32:06 | Merged the MKV/sliding-HLS effort into main. |
| media1's inspected container start | September 20, 00:28:22 | The running build contained the regression. This timestamp identifies the inspected deployment, not the first possible affected deployment. |

The reservation was introduced to prevent concurrent producers from all
starting against apparently empty scratch. That is a valid admission problem.
Extending the full producer reservation through read-only retirement is the
behavior this proposal changes.

### 2.3 Source anchors for review

All symbols and line numbers below refer to the evidence base, not whatever
branch happens to be open in the shared checkout. Re-resolve them on the
implementation base. The shared checkout predates the web source split, so
the split-file paths should be read from the recorded commit:

```bash
git show a14143684:crates/plurxd/src/transcode.rs
git show a14143684:crates/plurxd/src/web/player/transport.js
git show a14143684:crates/plurxd/src/web/player/decode-tiers.js
```

| Source at the evidence base | Review anchor |
|---|---|
| `crates/plurxd/src/transcode.rs:1348` | `ROLLING_SCRATCH_IN_FLIGHT_BYTES`, `RollingScratchReservation`, and its `Drop`. |
| `crates/plurxd/src/transcode.rs:6307` | `Session::scratch_reservation` survives retirement. |
| `crates/plurxd/src/transcode.rs:25890` | `reserve_rolling_scratch`: full configured ceiling plus 64 MiB, serialized admission, unclassified refusal string. |
| `crates/plurxd/src/transcode.rs:26000` | `global_flow_bytes`: live and retired entries use reservation floors; provisional reservations are added separately. |
| `crates/plurxd/src/transcode.rs:6880` | `prepare_retired_object_promise`: grace includes playlist duration and segment duration, and preserves existing later deadlines. |
| `crates/plurxd/src/transcode.rs:3473` | `spawn_retired_presentation_cleanup_owner`: waits for grace, deletes, retries failed cleanup, removes the exact retired entry. |
| `crates/plurxd/src/transcode.rs:4656` | `spawn_copy_reader_owner`: an independently spawned Rust reader can still write after FFmpeg has been reaped. |
| `crates/plurxd/src/http/hls.rs:3858` | `session_start_error`: recognized capacity errors become 503; this scratch string does not. |
| `crates/plurxd/src/web/player/transport.js:313` | `nudge`: combines pointer taps using `contractTiming("desktop_hotkey_coalesce_ms")`, 350 ms at this base. |
| `crates/plurxd/src/web/player/transport.js:690` | `seekTo`: direct/immutable VOD seeks locally; rolling HLS requests a media change. |
| `crates/plurxd/src/web/player/decode-tiers.js:1115` | `requestPlaybackMediaChange` and `executePlaybackMediaChange`: pending desired route, stale-response fencing, predecessor retained until successful create. |
| `crates/plurxd/src/web/player/session.js:174` | `playbackControlSnapshot`: any `controlSeek` becomes attached-session seeking state. |
| `crates/plurxd/src/playback_control.rs:8352` | `RollingStartupState::observe_control`: seeking clears the presentation baseline; the 30 s deadline remains anchored to first serving. |

## 3. Repeated +10 taps — what happens and what must be proved

### 3.1 Coalescing reduces requests but does not remove the regression

Pointer taps less than 350 ms apart update one pending target and reset one
timer. A burst of three taps can become one +30-second seek. `seekTo` also
has a 100 ms stale-intent check on its ordinary execution path. Neither is a
server storage guarantee, and the pending-open branch can execute before
that 100 ms wait.

Taps far enough apart to commit individually can create successive rolling
sessions. This path has **no local-buffer exception**: a buffered destination
does not make an ordinary rolling-HLS seek stay in the same session. The
earlier conversational suggestion that it might was too broad. Direct play
and immutable VOD have a different local-seek path.

Keeping an old HLS object readable does not require keeping its producer
running or reserving the producer's maximum future working set.

### 3.2 Repeated taps lose the delta and create duplicate pending streams

`nudge` uses `_seekPending` when it exists; otherwise it uses `pbPosSec()`.
Once a pending seek commits, `_seekPending` is cleared. If the replacement
has not attached, the media element can still report the old position.

The original partial-function probe produced **110, 110** from a fixed
100-second media position. The supplied review repeated the case with the
real `nudge`, `commitPendingSeek`, `seekTo`, `beginPlaybackControlSeek`,
`positionForPlaybackIntent`, `pbPosSec`, `restartPendingPlaybackOpen`, and
`requestPlaybackMediaChange`, stubbing the executor to remain pending.
Three committed taps produced **110, 110, 110**, with three intent sequences
and three create executions. This is an adapter reproduction, not a physical
browser run. The incident's 108.566 → 173.974 seek was a roughly 65-second
jump and is not itself evidence of this lost-delta case.

Each tap during a pending change re-enters `executePlaybackMediaChange`.
Generation fencing prevents old results from attaching, but earlier creates
remain capable of consuming full reservations until they resolve and are
released. A stale result is not the same as a cancelled producer.

**Proposed input contract:** while a viewer-owned seek is pending, another
relative seek advances the latest desired destination. It must not reset its
base to the outgoing picture. Keep the existing coalescing interval, clamp
rules, cancellation semantics, and newest-intent ownership. Reuse the
existing desired intent and position helper, adjusted for the ownership split
in §4.6; do not add a second position accumulator with a separate lifetime.
Cover keyboard, pointer, direction reversal, close, and title change.

**Required duplicate suppression (R2):** do not execute a second create for
the same in-flight change. Compare the computed target and complete requested
recipe/identity, not position alone: an audio, quality, subtitle, or file
change at the same position remains a different request. An explicit retry
after completion/refusal is also a new attempt. Coalesce unchanged pending
requests before `streamGeneration` invalidates the original attempt. Tests
must assert execution counts as well as targets and final attachment.

### 3.3 Real storage pressure remains possible

For clustered make-before-break, budget for the incumbent, **every** pending
or abandoned-but-unsettled create, and real retained bytes. Two producing
reservations are a controlled scenario with one successor, not a universal
upper bound. The legacy path retires before reserving; test both paths.
For twenty retired generations of 128 MiB and exactly one successor:

```text
2 × 2.0625 GiB + 20 × 0.125 GiB = 6.625 GiB
```

That controlled workload fits an 8 GiB budget and must not fail merely
because twenty seeks occurred. The 128 MiB is a test input, not a measurement
of the incident. Higher-bitrate media, slower cleanup, other viewers, or
more generations can legitimately exhaust the same cap.

The clustered calculation is `(1 + pending_creates) × R + retained_bytes`,
where an abandoned create still counts until its actual settlement. Deduping
identical requests removes one source of amplification; changing targets
can still overlap. Real pressure must fail truthfully while the incumbent
continues and obsolete work settles. In the legacy retire-before-reserve path
the equivalent formula is `pending_creates × R + retained_bytes`.

**4K matters.** At a sustained 80 Mb/s, 360 seconds of retained output is
approximately 3.6 GB (3.35 GiB). Two such generations plus two full producer
reservations need about 10.83 GiB, exceeding 8 GiB even after the accounting
repair. At 10 Mb/s the same modeled duration is 450 MB. These are sizing
examples, not measured per-generation inventories or upper bounds: variable
bitrate, extra tracks, actual window length, and promise duration matter.
Promise duration describes how long files remain readable, not by itself how
many seconds of media are stored.

Paul selected shorter retention after an exact same-viewer handoff or
explicit release (R3), with outstanding reads protected, and smaller initial
reservations with enforced growth (R4). Until those contracts are implemented
and tested, the fixed-reservation repair must not be advertised as solving the
high-bitrate repeated-seek class or supporting four concurrent rolling
producers. Three full default reservations are the limit even without seeks.

## 4. Proposed accounting contract — retire future capacity, retain bytes

### 4.1 One charge follows each exact session incarnation

Use one authoritative scratch charge per incarnation, independent of whether
the session is provisional, live, retired, or awaiting deletion. Adapt the
existing permit/accounting implementation; do not add a competing budget.
The states below are proposed semantics, not existing Rust type names.

| State | Charge | Allowed transition |
|---|---|---|
| Provisional / producing | At least the admitted reservation, or greater known actual usage | Transfer ownership without releasing/reacquiring the charge. |
| Retiring, writers not settled | Keep the producer charge | Confirm process death and all associated scratch writers settled. |
| Retained, writers settled | Complete measured retained inventory | Keep normal grace, or the explicitly authorized release grace from §4.7. |
| Deleting / cleanup failed | Bytes not yet proven reclaimed; retain the last conservative charge on uncertainty | Successful cleanup reduces the charge; failure cannot make capacity appear. |
| Released | Zero | Names have been removed and accounted reader pins have closed; idempotent accounting release. |

The charge must survive registry removal and HTTP cancellation. Final `Arc`
destruction is a backstop, not the event that normally returns capacity.
Conversely, a session disappearing from a registry is not proof that its
files disappeared.

### 4.2 Confirm all writers before reducing the reservation

FFmpeg's confirmed reap is necessary but insufficient for native copy
segmentation. The Rust `copyseg::run` worker drains its pipe and writes the
last segment/playlist independently. In the incident, the copy reader's
completion log followed the retirement-settled log.

Add an explicit completion barrier for scratch writers. Its ownership must
cover the interval **before** pipe installation through final reader exit,
including cancellation, panic, spawn failure, and executor handoff. Registering
the barrier only after spawning leaves a race where retirement sees no writer.
Normal actor rejection of a late reader classification is not writer-completion
evidence; the worker itself must have finished. C's classification was rejected
as `SessionEnded` after its reap. The barrier belongs to the actual worker,
installed before spawning; it must also preserve the owner's existing pipe
lifetime ordering through `classify_copy_producer_exit_before`. A classification
rejection cannot cancel or skip the writer-completion handoff.

Inventory the other writers and mutators: FFmpeg's direct HLS output,
publication/playlist writes, subtitles that share the directory, and detached
retention cleanup. A retirement fence must exclude any new write after the
final measurement. A stalled writer keeps its conservative charge and reports
that reason; it does not obtain a free reservation by timing out.

Do this work in the existing cancellation-independent retirement/cleanup
ownership path. Do not hold the global admission lock across process waits,
filesystem I/O, actor calls, or a grace timer. Do not delay the actor's terminal
decision just to finish disk accounting.

### 4.3 Measure once quiescent, then convert atomically

Reuse `Session::refresh_scratch_bytes` after writers settle to measure the
complete remaining scratch inventory:
advertised segments, grace segments, staged output, init, playlist, temporary
files, and renamed garbage. A directory read error or incomplete scan must
retain the prior conservative charge. Do not interpret a stale `live_bytes`
sample as proof of a final inventory. The scanner already preserves the last
sample on read/stat failures; expose an explicit success/result or measurement
generation so the caller can distinguish a completed final scan from an old
value. Do not introduce a second scanner with different inclusion rules.

**Units:** the existing scanner sums `metadata.len()` of regular files. This
is apparent file length in the namespace, not allocated blocks or physical
disk reclamation. Sparse test fixtures therefore work. At the evidence base,
session scratch is flat. Confirm that in one implementation-receipt sentence,
including regular-file/symlink handling; this is not a new design work item.

Commit the conversion as one admission-accounting transaction. Readers must
observe either the old producer charge or the final retained charge, never
an uncharged gap. A transfer between live and retired maps must not charge one
incarnation twice. An unrelated start must not use the conversion twice.

**The ledger is required.** The existing sum is provably non-atomic: read A
from the live map, let retirement move A under both map locks, then read A
again from the retired map. A is counted twice. Its reservation is also
subtracted twice when deriving provisional usage, whose zero clamp can hide
genuinely provisional work. The admission gate only serializes admissions;
it does not serialize those folds with registry transfer. This defect is
established from the lock ordering, not just a hypothetical resize race.

Let the existing permit carry an exact incarnation and mutable charge in one
ledger under the same short accounting gate used by admission. Admission sums
ledger entries exactly once; registry membership is not part of the charge
formula. Reserve, grow, retire, resize, and release have linearization points
in this ledger. Compute the ahead-byte pacing view separately. A status
snapshot must use the ledger's generation or same consistent snapshot so its
categories add to the admitted total.

After cleanup, the current code removes the retired entry and clears its
`live_bytes` but leaves its permit in `scratch_reserved_bytes`. Until the last
session Arc drops, that residue reappears as **provisional** usage. Eliminate
that subtraction-based inference. Only an actual not-yet-published start may
be called provisional; cleanup residue has its own exact ledger state.

The proposed charge is measured actual bytes, even if they exceed the original
reservation. Never clamp an overrun down to its reservation. This repair must
not weaken producer bounds or claim to prove every existing overshoot bound.

### 4.4 Cleanup owns namespace bytes and open-reader pins separately

Reservation conversion alone does not change `serve_until`. Normal retired
objects remain readable through existing grace. Only the explicit R3 release
contract in §4.7 may shorten it. Neither grace reads nor release acknowledgements
may renew a retired actor or restart its producer.

Release the namespace charge explicitly after successful cleanup, once, even
if a status reader still holds an unrelated `Arc<Session>`. Failed
cleanup stays owned and charged for retry, including unregistered starts.
Handle an already absent directory idempotently after proving writers cannot
recreate it.

An open response handle can retain an unlinked file's allocation. The directory
scan cannot see it. Track each exact media object's apparent length while
linked or pinned by an accepted read, counting it once even for concurrent
range requests. Unlink moves it from namespace ownership to reader-pin
ownership if readers remain; the final handle close releases that charge.
Cancellation, EOF, and failed responses must all settle the pin. An unrelated
session Arc is not a reader pin. Reuse an existing response-lifetime owner if
it already supplies these guarantees, rather than adding a second one.

These are conservative **logical-byte** charges. Do not label them `st_blocks`
or proof of physical free space: compression, sparse files and filesystem
allocation differ. Physical free-space sampling remains a separate guard.

### 4.5 Classify true refusals through the existing capacity path

Route the scratch-admission refusal through the existing `capacity_error`
classification, or its established typed equivalent, so `session_start_error`
returns **503 Service Unavailable**. Exercise the real returned error through
the HTTP mapping; a test that constructs an already-prefixed string alone
does not catch this regression.

Retain requested, charged, and configured byte counts in server diagnostics.
Distinguish producer reservations from retained actual bytes and pending
writer/cleanup charges. Avoid calling the sum physical disk usage.

The media-change path preserves the predecessor attachment at create refusal,
but the existing control state can still expire its actor shortly afterward.
The §4.6 repair is therefore mandatory. A true capacity refusal must leave a
clear retryable notice about the destination, not mark the movie corrupt,
pretend the seek completed, or stop the healthy incumbent later.
The current change-context policy intentionally does not retry automatically;
changing 500 to 503 alone does not add an automatic retry. A new wait queue or
automatic retry policy is outside this repair and would need a separate
bounded policy with latest-intent cancellation.

### 4.6 Separate desired replacement intent from attached-session observations

Keep the desired destination and recipe for UI, subsequent relative input,
and Retry. Bind a seek execution separately to the exact attachment/session
it will mutate. An unexecuted replacement destination must not become
`render_state="seeking"` or `seek_target_ms` on the incumbent reporter.

This applies from the beginning of a replacement, not just after its refusal.
While a new create waits or fails, the incumbent snapshot reports its actual
position, render state, and attached selection. Once a successor attaches,
its reporter owns the destination/landing state. Direct/immutable-VOD seeks
that actually mutate the incumbent element retain their existing seeking
semantics. Coordinate this split with `positionForPlaybackIntent`: simply
clearing `controlSeek` and losing Retry's target would fix one defect by
reintroducing another.

Fence the snapshot at capture/send time by attachment/session generation so
an asynchronous old seek notification cannot contaminate a new attachment.
Notify the incumbent reporter promptly when a refused or superseded execution
is detached from it. Preserve honest failure/waiting/paused states; do not
force `Rendering` just to bypass startup expiry.

The server regression must drive the existing actor with these real scoped
snapshots: two accepted rendering observations with at least 250 ms of media
progress establish `Presented`; foreign desired targets are absent. The
actor must then survive beyond the original 30-second startup deadline while
the replacement remains pending/refused. Keep the expiry for a genuinely
unpresented stream and for an actual local seek that never lands. Do not
disable the deadline globally or accept `v.playing` alone as proof. No wire
extension is needed: the incumbent reporter already posts to that session's
control URL with its `generation` and `control_epoch`. Correct the client
snapshot and drive the real server actor in the regression. Remove foreign
intent from that exchange rather than asking the actor to infer it.

### 4.7 R3 — shorter grace after an exact same-viewer handoff or release

**Direction selected by Paul, September 20:** propose shorter retention with
explicit safeguards. This changes the first draft's blanket exclusion of
grace policy from scope.

Use the existing **`DELETE /api/v1/hls/{session}`** capability-authenticated
release signal. At the evidence base `delete` (`hls.rs:4165`) reaches
`release_session` (`:4309`), obtains the exact durable terminal route through
`end_media_session_for_release` (`:4467`), and projects it to the owner. Remote
stop uses the resolved `owner_epoch`; the public DELETE does not contain an
`expected_owner_epoch` supplied by the browser. Reuse that coordinator and its
incarnation fencing rather than introducing an attachment-acknowledgement
endpoint or treating a successful create as proof of attachment.

Apply the grace update only to the exact retired incarnation that coordinator
resolved, within its immutable user/playback scope. A stale or unknown ID,
wrong epoch/incarnation, or another viewer's release is a no-op on this
incarnation's grace. The endpoint retains its existing idempotent semantics;
do not invent a public epoch query parameter for this repair.

On an eligible event, record one non-renewing release deadline:

```text
release_deadline = min(original_deadline,
                       accepted_release_time + release_allowance(client_class))
```

Use named client-class policies, with this source-checked disposition:

| Client/release path | Verified ordering at the evidence base | Release allowance |
|---|---|---|
| Confirmed hls.js predecessor teardown | `retirePlaybackPredecessor` destroys hls.js before `releaseSession` sends DELETE (`decode-margin.js:399–408`). | One session segment target, **16 s** at this base, with accepted reads pinned beyond it. The destroyed instance's seven-retry/8 s-backoff policy does not add a new post-destroy retry window. |
| Safari native HLS / AirPlay | On attach, `resetMediaSource` removes the old source and calls `load()` before `setPlaybackMediaSource` installs the successor. Native teardown is known, but predecessor DELETE can precede that reset; the in-flight/retry tail is not bounded. | Apple-like conservative class: keep original grace until release ordering/retries are bounded. Do not identify this class merely as “web” or “Safari.” |
| Apple replacement | `PlayerController.swift:4593` awaits `release(session: superseded)` **before** `replaceCurrentItem(with:)` at `:4620`. The old item still exists while DELETE runs. | Keep original grace. Moving release after item replacement is a possible client repair, but does not itself prove AVFoundation's retry bound. |
| Apple stop | `PlayerController.swift:4057` detaches the item before scheduling session end. | Ordering is safer, but no finite AVFoundation retry bound was established by reading this source; retain original grace for now. |
| Unknown class, unaudited release site, or no DELETE | No proven teardown/retry contract. | Original grace. |

If the server cannot distinguish possible classes from retained session
metadata, use the largest applicable allowance; an unbounded/unknown class
means original grace. Specify how confirmed hls.js teardown is identified
before enabling its shorter window. The existing DELETE carries no transport
class, and not every web release site has the audited predecessor ordering.
Do not guess from the user agent, and do not change the control wire.

**Proposed Q7 mechanism, from review three:** add an optional `transport`
field to the session **create** body and retain it as exact-session metadata.
The browser already selects native versus hls.js through
`PlaybackPolicy.hlsTransport` before creating the copy session. Omitted or
unrecognized transport retains original grace. This is create metadata, not
a change to the control snapshot. Propagate it through remote placement,
replay and owner handoff without upgrading a legacy/unknown session. Audit
all release sites for a client that opts into the hls.js contract; knowing
transport alone does not prove destroy-before-DELETE. A transport fallback
must not leave stale short-grace eligibility. Unit B owns that mechanism's
implementation proof; the handoff specifies the compatibility tests.

The 20 s server `SEGMENT_WAIT` is not a client retry allowance. Duplicate
DELETEs must not extend the recorded deadline. Already-open reads can outlive
every class's release call; they remain pinned and charged separately.

Existing accepted reads keep their object pins and complete safely. New reads
inside the release window follow read-only grace rules; after it, the exact
old session returns the established retired/gone response. Do not alias old
URIs into the successor. Cleanup may remove unpinned objects once eligible;
pinned bytes remain charged until readers close. This protects in-flight
requests without retaining the whole playlist's worth of unneeded objects.

Finite short grace still permits temporary pressure under a seek storm. The
4K acceptance must measure backlog at the selected allowance and tap cadence;
do not promise unlimited immediate seeks or early deletion of another
viewer's data.

### 4.8 R4 — reserve the initial burst and authorize growth before writes

**Direction selected by Paul, September 20:** propose smaller initial
reservations with enforced growth. Keep the global 8 GiB default for the
comparison; do not present three concurrent rolling producers as an accepted
product limit.

**Two different enforcement paths exist.** Native copy writes through
`copyseg::Writer::write_segment` and `publish_file` (`copyseg.rs:222–264`),
where Rust has the complete byte slice before the temporary-file write.
Direct/transcoded HLS instead uses FFmpeg's `-f hls` and
`-hls_segment_filename` paths (`transcode/mod.rs:1689` and other argv builders).
There is no existing Rust grant hook before those FFmpeg writes. Its existing
`Bytes`/`Global` hold suspends the exact child through process control
(`SIGSTOP` on Unix); preserve that mechanism rather than pretending the two
paths have identical write ownership.

**Native copy:** acquire a ledger grant for the complete next file before
`publish_file` writes its temporary file, including init and playlist output.
Account for old/new playlist overlap through atomic rename. Use the actual
slice length: `Limits.max_bytes` is a cut-policy threshold, not proof that
every segment fits exactly 64 MiB. The source explicitly allows the fragment
that crosses that threshold. A refused grant backpressures the Rust writer;
suspending FFmpeg alone does not stop a pipe reader from draining and writing.

**Direct FFmpeg candidate:** maintain `charge = actual + envelope`, refreshed
by a ledger grant at each flow evaluation. Grant before resuming; refuse
growth by retaining the existing hold. Periodic evaluation is a valid control
point. What still needs proof is the amount the running process can write
between measurements and confirmed suspension, including startup and delayed
evaluation. The re-review suggested this normal-operation estimate:

```text
estimated_envelope = readrate × output_bitrate_bytes_per_second × evaluation_gap
                      + bounded_in_flight_output
```

At the base, default readrate is 2.0 and the repair timer is 15 s. These
numbers do **not yet prove a hard envelope**:

- `HLS_BURST_SECS_DEFAULT = 90` permits the initial input burst to run
  flat-out. The argv builder emits `-readrate_initial_burst` before the normal
  readrate option. Remaining burst and buffered encoder/muxer output belong
  in the allowance; steady-state pacing alone does not cover them.
- Declared or measured bitrate is an estimate, not a maximum. Five times the
  estimate produces five times the estimated growth during the same interval.
  Input media-time pacing also is not a per-write output-byte quota.
- `FLOW_CONTROL_REPAIR_INTERVAL = 15 s` is the requested timer cadence, not
  a proven maximum evaluation gap. `reap_loop` awaits lease checks, retirement,
  filesystem refresh/GC and signals serially. A slow operation can extend it;
  client fetches need not arrive to supply a faster evaluation.
- Successful suspension prevents later userspace work but does not undo
  writes already issued. The allowance must include the write tail and
  signal/observation latency. Prove the incident path on Unix and preserve
  the existing Windows process-control abstraction; see platform scope below.

Thus this candidate may use an envelope proven from enforced output-rate,
burst and evaluation/suspension bounds, or use a hard writable quota/output
boundary. Merely multiplying declared bitrate by 15 s cannot pass as hard
enforcement. A quota failure must be classified and cleaned up; it must not
be reported as a successful hold if it actually killed the producer. This
qualification is an explicit disagreement with re-review finding 1's claim
that the existing numbers already prove the bound.

**Startup must cover the effective publish gate plus a segment.** A small
grant cannot depend on client drain before anything is published. At the
base the copy writer's `COPY_PUBLISH_GATE_SECS` is **12 s**, its preferred
ceiling is **15 s**, and its truthful target is **16 s**. There is also a
later rolling publication gate: `ROLLING_INITIAL_RUNWAY_MS = 48,000`, scaled
by `rolling_initial_runway_ms(playback_rate)`. At 1×, sizing only `(12 + 15)`
seconds misses the actual 48-second startup requirement.

Use this initial-sizing contract, with units of output bytes:

```text
effective_gate = max(all enabled writer and served-playlist startup gates)
startup_media = effective_gate + maximum_complete_segment_duration
known-rate_initial >= estimated_bytes(startup_media) + enforcement_envelope
unknown-rate_initial = 256 MiB + enforcement_envelope  # bootstrap, not safety proof
```

For the current copy/rolling path at 1×, use at least **48 + 16 = 64 seconds**
of output in that known-rate sizing, plus enforcement overhead. At a modeled
80 Mb/s this is 640 MB before the envelope. Account additionally for any
uncontrolled initial burst exceeding that amount; at default 90 s that is
900 MB of modeled output. Read effective configuration and playback rate;
include all tracks, temporary files and playlist overlap. Short-title ENDLIST
remains an explicit escape from the duration gate.

The 256 MiB unknown-rate bootstrap is not a hard floor or a bound on needed
space. An unknown-rate 80 Mb/s source, or one declared at 16 Mb/s and actually
producing 80 Mb/s, must obtain further grants before consuming them. If
insufficient capacity prevents the startup gate from ever opening, perform
bounded classified refusal and cleanup; never hold forever waiting for a
client that has no playlist to drain. Enforced writes, not bitrate guesses,
must prevent the overrun in this test.

Bytes cannot be granted twice across overlapping writes. Cancellation returns
unused grants, while unlink and reader close release used charges through
the same ledger. Keep publication progress, startup deadlines and legitimate
flow holds distinct so a stopped writer is not diagnosed as making progress.

**PR C design gate:** exact native-copy grants are implementable at the
identified write boundary. The direct-FFmpeg envelope still requires a proven
enforcement bound or quota mechanism. Until then, retain its conservative
reservation as an explicit interim limitation; do not call a copy-only change
completion of R4. This gate must not delay the independent incident repair
work in PR A, but **Paul selected one combined promotion**: do not release A
separately while C is unresolved. Qualify every supported rolling writer
before claiming the overall multi-producer repair complete.

**Platform scope:** Unix supplies the incident/runtime acceptance here.
Review three described the Windows port as unbuilt, but the
[Windows status](../features/WINDOWS-PORT-STATUS.md) records it as merged,
with D2 approved and native proof still open. `process_control.rs` already
implements Windows suspension/resumption through `NtSuspendProcess` and
`NtResumeProcess`. Preserve its compilation and conservative safety behavior;
do not assume a Unix signal proves Windows enforcement. Record any unproved
Windows growth mode explicitly and keep it conservative. The existing
Windows native-receipt work remains with that port; its eventual activation
of smaller reservations must satisfy this same write-bound contract.

## 5. Fix boundary — accounting, incumbent survival, and seek capacity together

1. **Server accounting and lifecycle:** replace the registry-derived sum with
   exact ledger entries; add writer-completion evidence, reservation
   conversion, explicit cleanup release, diagnostics, and 503 classification.
2. **Repeated-input contract:** base relative seeks on the desired target,
   suppress identical in-flight creates, preserve newest-intent ownership,
   and settle every abandoned admitted create. Assert counts and positions.
3. **Incumbent survival (R1, required):** separate desired replacement intent
   from the attached reporter's observations during pending and refused
   creates. Include the web snapshot and server startup-actor regressions;
   keeping the picture attached for the instant of refusal is not acceptance.
4. **Seek and concurrency capacity (R3/R4):** implement exact same-viewer
   release grace and smaller initial reservations with hard growth limits
   after the §4.7–4.8 design gates are resolved. Keep the global default for
   comparative acceptance; demonstrate both high-bitrate repeated seeks and
   more than three low-bitrate rolling producers when their charges fit.
5. **Regression evidence and documentation:** bind the new tests to the
   repository validation catalogue and update the playback/operations
   descriptions when the behavior changes.

Do not disable the scratch cap, raise its default to mask the defect, erase
media before its applicable promise/pin permits cleanup, kill the predecessor
to make a clustered replacement fit, introduce a second seek controller,
add rolling buffered-seek reuse, change immutable-VOD behavior, or requeue
production indexes as part of this fix. Do not weaken startup proof or deadline
semantics for genuinely unpresented sessions. These would weaken guarantees
or solve another problem.

Implement from then-current main, preserving unrelated checkout changes.
With the accepted R3/R4 direction this is a coordinated effort: ledger,
incumbent/input ownership, and writer/grace enforcement are reviewable work
packages with overlapping files. **R6, selected by Paul after the re-review:
keep one combined promotion.** Use three review units:

| Review unit | Scope | Implementation gate |
|---|---|---|
| A — incident repair | Ledger, writer barrier, conversion, cleanup ownership, 503, incumbent survival (R1), desired-target accumulation and deduplication (R2). | §4.1–4.6 is ready to become the implementation contract; prove its lifecycle and real-actor regressions. |
| B — shorter retention | R3, exact DELETE incarnation, client-class allowance and pinned reads. | Resolve reliable release-class identification and bound each enabled class; retain original grace for unproved classes. |
| C — smaller growing reservations | R4, native grants and direct-FFmpeg enforcement, startup sizing and concurrency. | Prove direct-writer enforcement and startup liveness under unknown/underestimated rates; a copy-only result does not complete C. |

Integrate on one temporary effort branch, with each task based on its current
tip and reviewed back into the effort. A can be implemented and reviewed
while B/C's technical gates are resolved. It is not a separately releasable
promotion in this plan. Follow the
[contributor workflow](../../AGENTS.md) and
[development pipeline](../DEVELOPMENT_PIPELINE.md); qualify the combined
candidate against then-current main before the single promotion. The
re-review recommended separately shipping A, B and C; the owner selected
combined promotion instead. This document does not authorize deployment or
bypass review.

Record the UTC date and exact candidate when A first passes its required
checks, then its current revalidation state. Until combined qualification and
a later deployment, this effort supplies no production repair; keeping that
date makes the wait for B/C visible without silently changing R6.

## 6. Acceptance — test a seek sequence, not only two simultaneous starts

Existing tests at the evidence base cover concurrent reservation admission,
non-playlist file accounting, and read-only retired objects. They do not
establish that a repeated replacement sequence sheds unused reservations.
Keep those tests; add the combined lifecycle coverage below.

| Case | Required proof |
|---|---|
| Reproduce the incident | Default settings, one playback and three successive replacements; the old code refuses the fourth outstanding reservation. The repaired code admits it when actual retained inventory fits. |
| Twenty committed seeks | Exercise real reserve/register/retire/cleanup transitions with twenty 128 MiB retired inventories and exactly one incumbent/successor pair. Keep normal grace pending to isolate conversion from R3. Every fitting admission succeeds; promised objects remain readable. Sparse fixtures test apparent lengths. |
| Delayed copy writer | Reap FFmpeg while the Rust writer is paused before its last write. The reservation stays conservative. Release the writer, measure its tail output, then reduce the charge. No file is deleted under the writer. |
| Measurement failure | Inject read/stat failure after partial enumeration. No capacity is released from an incomplete measurement; retry can eventually settle it. |
| Real exhaustion | Enough actual retained bytes to exceed the budget still prevents admission. Send the real refusal through the HTTP handler and assert 503, not 500. |
| Map-transfer interleaving | Pause an admission after the old live fold, retire an entry before the old retired fold, and keep a genuinely provisional permit. The old implementation exhibits the inconsistent sum; the ledger counts each incarnation once without hiding provisional charges. |
| Concurrent viewers | Race reserve, grow, live-to-retired transfer, charge conversion, and cleanup at the cap boundary. No undercount, double release, or duplicate charge for one incarnation. |
| Request cancellation and failure | Cancel a start before registration, after pipe installation, and during retirement; also fail/panic the copy worker. Cleanup retains ownership and the charge until its evidence permits release. |
| Cleanup failure / lingering reference | Failed unlink retains charge. Successful deletion with no reader pins releases it despite an unrelated retained session Arc. It cannot reappear as provisional. Retry and duplicate completion cannot subtract twice. |
| Open file response | An old media response remains open while grace expires and unlink succeeds. The object stays readable and logically charged once until its last pin closes. Multiple ranges, cancellation, and EOF all settle correctly. |
| Pointer burst | Three +10 taps inside `desktop_hotkey_coalesce_ms` produce one +30 target and one committed change. Read the shipped constant rather than duplicating 350 in a test. |
| Taps during pending replacement | Keep the outgoing picture at 100 seconds. Separate +10 commits yield desired targets 110, 120, 130. Record one execution per distinct committed target under the existing policy and no repeated execution for an identical pending recipe/target. Late creates never attach over 130; every abandoned admitted session is released. |
| Identical target / changed recipe | Repeating an identical pending change leaves one execution and preserves its attachment ownership. Changing tracks/quality at the same target still executes; explicit Retry after refusal is not deduped away. |
| Abandoned creates in flight | Keep several distinct creates unresolved. Every one remains charged until its real settlement, even after its browser generation becomes stale. Account for more than two producer reservations. |
| Refused destination beyond startup | A real web fixture retains a refused desired target for more than 30 s while the incumbent presents. Its snapshot returns to actual `rendering` with no foreign target; Retry retains the desired position and recipe. Drive those observations through the server actor and assert `Presented` and no startup expiry. |
| Slow replacement beyond startup | Hold create unresolved beyond 30 s while the incumbent presents. From the beginning its snapshot describes the incumbent. The actor establishes presentation; old target notifications and the late create cannot contaminate a newer attachment. |
| Genuine startup failure | No accepted presentation progress still expires under the original budget. A native/local seek that does not land is not hidden by the replacement-intent fix. |
| Direction and lifecycle | Reverse direction, cancel a preview, close, or change title while a replacement is pending. No stale target, attachment, or provisional reservation survives. |
| Genuine capacity in the web UI | Preserve the incumbent and desired target; show a specific retryable refusal. Retry/new input uses the latest target and the existing request ownership rules. |
| Other presentation paths | Direct play and immutable VOD keep their local-seek behavior. Copy HLS and transcoded rolling HLS both obey the accounting transition. |
| Same-viewer release grace | Drive the existing DELETE coordinator for the exact retired incarnation and assert the shorter deadline is non-renewing. Stale/unknown ID or wrong resolved epoch/incarnation is a no-op on that incarnation's grace. Another viewer is unaffected. No DELETE and unproved client/release classes retain original grace; ambiguous classes use the largest allowance. Confirm hls.js destroy-before-release and preserve the conservative Apple/native cases. Accepted reads complete; post-deadline new reads are gone. |
| High-bitrate seek sequence | Generate or model 80 Mb/s output with a long incumbent window, then perform repeated +10 seeks with R3 enabled. Measure actual retained backlog, all pending creates, reader pins, final target and producer growth; the reported class is accepted only within a recorded cadence/cap/window. |
| More than three producers | Under the unchanged 8 GiB cap, start at least four low-bitrate rolling producers whose enforced initial charges fit. They start and present; growth respects the same global budget. Repeat across every supported writer path. |
| Underestimated burst / growth denial | Use unknown-rate 80 Mb/s and declared 16 Mb/s / actual 80 Mb/s output. The writer obtains a grant before consuming bytes, or backpressures without overrun. Cover temporary, init, playlist and native/direct-HLS paths, cancelled growth grants, a fragment crossing the native 64 MiB cut threshold, and a minimum segment larger than remaining capacity. |
| Startup cannot drain yet | Exercise the 256 MiB unknown-rate bootstrap and known-rate sizing with the effective rolling gate (48 s at 1×), truthful 16 s segment target, playback-rate scaling and short-title ENDLIST. Growth either reaches a publishable playlist or produces bounded classified refusal/cleanup; no indefinite hold waiting for an unpublished playlist to drain. |
| Direct writer enforcement envelope | Exercise the default 90 s initial burst, delayed repair-loop evaluation beyond 15 s, absent segment fetches, underdeclared bitrate and writes outstanding at suspension. Verify Unix enforcement and multiple producers; preserve Windows compilation and conservative behavior without claiming its pending native receipt. A nominal readrate × 15 s estimate does not satisfy this case. |

A owns the incident, ledger/lifecycle, input, incumbent and unchanged-path
regressions. B owns release grace and the high-bitrate retention backlog
acceptance. C owns growth, startup, direct-writer enforcement and the
more-than-three-producer acceptance. Shared concurrent-viewer and physical
seek coverage must be rerun against the combined candidate; passing A alone
does not qualify the selected promotion.

New tests must fail against the old implementation for the behavior they
claim to repair. Use barriers and explicit completion events for races,
rather than sleep-based timing guesses. Keep HTTP classification, accounting
concurrency, and browser ownership assertions separate enough to localize a
failure.

Suggested verification commands, using real test names once implemented:

```bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo fmt --all -- --check
rustup run 1.97.1 cargo check -p plurxd --all-targets --locked
rustup run 1.97.1 cargo clippy -p plurxd --all-targets --locked -- -D warnings
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls --locked
rustup run 1.97.1 cargo test -p plurxd --bin plurxd scratch_charge_ --locked
node --test tests/playback/seek-control.test.js tests/playback/web-policy.test.js
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

Use `scratch_charge_` as the common prefix for the new accounting/lifecycle/
HTTP regressions; it is a proposed filter and must not be reported as passing
if it matches zero tests. Add the incumbent-startup and full-adapter commands
explicitly to the implementation receipt. Preserve the existing manual-drop
reservation test: it correctly tests admission, while the new lifecycle tests
must prove conversion without manually dropping permits. Recheck against the
exact intended base before pushing.

For physical acceptance, use Safari against a forced rolling-HLS fixture:
tap +10 rapidly, then tap at one-second intervals during slow replacement,
and repeat after playback settles. Record desired and presented positions,
producer count, reservation bytes, retained bytes, capacity refusals, and
cleanup completion. Include a long-playing 4K copy, more than three low-bitrate
producers, slow/failed creates beyond 30 s, and a deliberately small cap to
distinguish legitimate pressure. Never mark this accepted from unit tests or
disk inspection alone.

## 7. Review disposition, selected directions, and remaining technical gates

The supplied September 20 first review returned **APPROVE WITH CHANGES** against
the same evidence base. Its adapter probe is reviewer-supplied evidence;
the source paths and retained incident logs were rechecked while revising
this document. No new production inspection or mutation was needed.

| Finding | Disposition in this revision |
|---|---|
| 1 — refused target expires the incumbent | Accepted as a blocking part of the repair, including the slow-create case (§2.1, §4.6, §6). Browser presentation and server `Presented` are distinguished. |
| 2 — lost delta and duplicate creates | Accepted; full-adapter result recorded and both target/count assertions required (§3.2). Dedupe includes recipe/identity so same-position track changes remain possible. |
| 3 — non-atomic global sum | Accepted as proved by the lock interleaving; one authoritative ledger is mandatory (§4.3). |
| 4 — post-cleanup provisional residue | Accepted; explicit release and exact ledger categories replace inferred provisional usage (§4.3–4.4). |
| 5 — high-bitrate retained media | Accepted as a product acceptance gap. The bitrate examples are sizing scenarios, not measured bounds; same-viewer release does not prove absence of readers (§3.3, §4.7). |
| 6 — three-producer ceiling | Accepted; R4 changes the proposed sizing policy (§4.8). The claim that measured accounting alone closes the write race is rejected; grants must precede writes. |
| 7 — writer/classification ordering | Accepted; the worker owns completion and the classification rejection does not release it (§4.2). |
| 8 — reuse measurement | Accepted; expose scanner completion rather than duplicate scanning. Namespace apparent lengths and reader pins are separated from physical allocation (§4.3–4.4). |
| 9 — timeline | Accepted; all four refusals, presentation, a5 supersession, terminal expiry, and user dismissal are recorded (§2.1). |
| 10 — naming | Applied the `media1` convention; private title/path and raw session IDs are omitted. |
| 11 — constant and test filter | Accepted; tests use the shared coalescing constant and proposed `scratch_charge_` filter (§3.1, §6). |

The second review also returned **APPROVE WITH CHANGES**, identifying
§4.1–4.6 as ready for an implementation contract. Its R3/R4 recommendations
were checked against the same source base; the distinctions below are part
of the revised contract, not claims of new runtime behavior.

| Second-review finding | Disposition |
|---|---|
| 1 — distinct writer enforcement mechanisms | Accepted the missing direct-FFmpeg write hook, existing process hold and exact native-write grant boundary. Qualified the claimed hard envelope: average bitrate, a 15 s timer and normal readrate do not bound the 90 s initial burst, delayed evaluations or write tail. Native 64 MiB is a cut threshold with a crossing fragment, not a strict file maximum. C requires an enforced bound or quota (§4.8). |
| 2 — startup sizing | Accepted gate-plus-segment sizing and the unknown 80 Mb/s test. Corrected the effective gate to include rolling publication's 48 s at 1× and the truthful 16 s target; 256 MiB is only an unknown-rate bootstrap and must grow safely (§4.8, §6). |
| 3 — release signal and allowance | Named the existing DELETE and actual durable-route/owner-epoch path; no public expected-epoch argument is added. Source inspection found Apple replacement DELETE precedes item replacement, while hls.js predecessor destruction precedes DELETE. Enable a 16 s allowance only for a reliably identified eligible hls.js release; retain original grace for native/Apple/unknown paths pending proof (§4.7). |
| 4 — control protocol | Accepted; existing session URL, generation and control epoch suffice. Removed the proposed wire-extension option and retained the real-actor regression (§4.6). |
| 5 — startup deadline anchor | Accepted; first validated playlist at 15:30:45.653 plus 30 s predicts 15:31:15.653, about 470 ms before the observed reap (§2.1). |
| 6 — delivery shape | Paul selected **one combined promotion**, retaining A/B/C as separate review units on the effort branch. A can proceed before B/C design completion; release waits for combined qualification (§5). |
| 7 — minor corrections | Added the legacy-path formula, exact-incarnation stale-DELETE no-op test, and flat-directory scanner audit as an implementation-receipt sentence (§3.3, §4.3, §6). |

**Third review: APPROVE, no blockers.** Fable withdrew round two's claim
that readrate × estimated bitrate × 15 s already proves a hard envelope.
The four follow-up notes are recorded as follows:

| Note | Disposition |
|---|---|
| 1 — Q7 mechanism | Adopted optional create-time transport metadata as B's proposed mechanism, with legacy fallback, remote/replay propagation and a release-site audit (§4.7). |
| 2 — Windows scope | Unix runtime proof is primary. Corrected the premise using the merged port status and existing Windows suspend/resume implementation; retain compile coverage and conservative behavior, with unproved native growth activation explicit (§4.8). |
| 3 — cost of combined promotion | Record A's first-green UTC date and exact candidate in the implementation receipt; preserve Paul's combined-promotion decision (§5). |
| 4 — native attach and expiry timing | Recorded native source reset and its unbounded retry tail. Qualified the proposed expiry-trigger inference: last renewal kind and a sub-second gap do not identify the claiming event (§2.1, §4.7). |

| Ruling | Recorded direction |
|---|---|
| R1 | Required repair: pending/refused destinations cannot poison the incumbent's presentation state. Included; no follow-up deferral. |
| R2 | Required repair: dedupe an identical in-flight create, with full recipe/identity comparison and explicit-retry distinction. Included. |
| R3 | **Paul selected:** propose shorter retention with explicit safeguards. Existing DELETE supplies the exact release; reliable client/release classification and unproved native retry bounds remain technical gates (§4.7). |
| R4 | **Paul selected:** propose smaller initial reservations with enforced growth. Exact native grants and effective startup sizing are specified; direct-FFmpeg enforcement still needs a proven bound or quota (§4.8). |
| R5 | Proposal retains manual retry and defers new automatic-retry policy, contingent on R1 preserving the incumbent. No new retry queue is implied by 503. |
| R6 | **Paul selected after re-review:** keep one combined promotion. Review A/B/C into the effort, then qualify and promote together; do not separately ship A (§5). |

The implementation contract and evidence must answer questions 1–6 for A.
Questions 7–8 are B/C design gates; they do not block A's implementation but
do block the combined promotion:

1. Does the writer barrier cover every way bytes can arrive after process
   reap, including start cancellation and the native copy worker?
2. Is charge conversion atomic with admission and registry transfer, and is
   the last known conservative charge retained on measurement failure?
3. Who owns bytes after grace, failed deletion, and unlink with an open file
   response? Can every charge be released exactly once without waiting for
   an unrelated session reference to disappear?
4. Do separately committed relative seeks advance desired position while an
   earlier create is pending, and do abandoned responses release their
   admitted sessions?
5. Does a pending/refused destination stay outside the incumbent's attached
   snapshot, including beyond the startup deadline? Does the actor still
   require genuine progress and expire genuinely unpresented streams?
6. Does the twenty-seek acceptance use actual retained bytes and real
   lifecycle transitions, rather than manually dropping reservation objects?
7. Does the proposed create-time transport metadata reliably identify the
   audited hls.js release path through placement, replay and fallback?
   Can every enabled shorter allowance be proved without shortening
   unknown/legacy/native promises?
8. What enforced rate/burst/deadline bounds or quota make direct FFmpeg's
   envelope safe? Do exact native grants and the effective startup-gate
   sizing permit progress or bounded refusal under unknown/incorrect rates?

**Evidence already collected:** production logs and read-only process/storage
inspection; git introduction history; source tracing; the partial `nudge`
probe and reviewer-supplied full-adapter reproduction; and a successful
baseline `cargo check -p plurxd --all-targets
--offline` using Rust 1.97.1 at the evidence base. Baseline compilation is
environment evidence, not proof of the proposed repair.

**Still unproved:** the repaired accounting and adapter behavior, writer
growth enforcement, release-grace compatibility and chosen constants,
regressions against a patch, physical repeated-seek/concurrency acceptance,
and release qualification. No Rust or web implementation was
changed during this investigation. The current deliverable is this review
proposal, its implementation handoff and their documentation-index entries.
