# MKV duration and Apple sliding HLS — root causes and reviewed repair

**Status:** Fable review addressed; implementation proposed, not built ·
**Written:** 2026-09-18 UTC · **Revised:** 2026-09-19 UTC ·
**Incident:** iPad playback of reference film F, file 6109.

Companion to the
[content-analysis RCA](CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md) and
[Apple pacing-hold RCA](APPLE-PACING-HOLD-FREEZE-ROOT-CAUSE.md).
The [Sol implementation handoff](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md)
owns task order, interfaces, tests, and integration. This document owns the
incident evidence, causal limits, and disposition of Fable's review.

Read §1–§4 for the diagnosis, §5–§9 for the repair rationale, and §15 for the
finding-by-finding disposition. The September 19 revision corrects the original
account of the terminal reopen and the retry policy. It does not claim that
every recorded delivery stall was a terminal AVPlayer failure.

## 1. Verdict — missing duration exposes two rolling-playback failures

The selected H.264 video has no embedded duration. The content index refuses
it as `index_completion_unverified`, which is terminal. Immutable VOD remains
unavailable until a changed completion policy and an explicit exact-identity
requeue succeed.

The rolling route has two related defects:

1. **Startup protection ends on server publication, before presentation.**
   A burst publishes 32 seconds in about one second; the time limit holds the
   producer at a 30-second target while the reopened client has not presented
   a frame. The client subsequently fetches 20 seconds but remains waiting.
   Its item ends with `AVErrorContentNotUpdated` / CoreMedia `-12888`.
2. **Steady-state hysteresis freezes playlist advancement for tens of seconds.**
   The 30-second reserve also becomes approximately the amount the client must
   drain or fetch before the producer resumes. Some clients tolerate this with
   their existing buffer; others stop fetching and enter a mutual wait.

```text
selected video has no embedded duration
  → terminal index refusal → rolling fallback
                              │
           ┌──────────────────┴────────────────────┐
           ▼                                       ▼
burst passes server-only startup floor      steady-state time hold
before client renders                      releases near exhausted reserve
           │                                       │
producer holds; native client waits         no new media in playlist;
           │                               some clients stop fetching
           ▼                                       │
reopen never presents; -11866 / -12888       delivery stalls → bounded reopens
```

The terminal reopen is evidence for a specific playlist-freshness failure.
It is not evidence that every preceding hold deterministically caused the
same native error. Updating the app alone cannot repair server publication.

Two further defects need correction in the same effort: the rolling target
duration can change, and removed segment URIs become unavailable too early.

## 2. Scope and evidence — separate measurements from inference

### 2.1 Revisions and review provenance

The incident server, called `media1` in evidence prose, ran
`a5454c4064cc5b1d03227b41a7a7ae319bdb3943`.
The original investigation used checkout `7cdbdd1a`. Fable reviewed against
`395ce5912d088572f1f63aa638129fc514ffc5f0`, reporting fresh read-only
telemetry, logs, container-label checks, and source probes.

This revision checks the relevant source contracts and cites Fable's additional
measurements explicitly; those live queries were not rerun for this revision.
The handoff base is locally available `origin/main` at
`a2d9c2fb7b26e142a39b70da8cc0e57bbbdf878a`. Between Fable's base and that
revision the relevant Rust files did not change. Sol must fetch and reverify
the current base before implementation.

The incident device was reported as Apple build 165, which already contained
PR #803's bounded delivery recovery. The original checkout declared 168,
Fable's declared 169, and the handoff base declares 170. These are dated
snapshots, not evidence of a server freshness fix. Build numbers are read from
[the Apple project](../../clients/apple/project.yml).

PR #364 established neutral evidence names. This document uses reference
film F and media1, omits private source paths, and retains file 6109 as the
exact incident identifier.

### 2.2 Selected-video duration is absent; other tracks can still have duration

The deployed probe returns this selected-video subset:

```json
{
  "streams": [{
    "index": 0,
    "codec_name": "h264",
    "time_base": "1/1000",
    "start_time": "0.000000",
    "tags": {}
  }],
  "format": {"duration": "6423.712000"}
}
```

There is no positive selected-video `duration_ts`, decimal `duration`, or
Matroska `DURATION` / `DURATION-eng`. Fable reports four subtitle streams
with duration 6423.712 seconds; video and audio lack one. This is not a claim
that every track in the file lacks metadata.

The resolver in
[`completion_expectation_from_probe`](../../crates/plurxd/src/fragindex.rs)
therefore refuses the index. Fable found the corresponding daemon outcome at
18:24:15 UTC and 16 rolling sessions with refusal `vod_index_pending`.

### 2.3 Packet bounds reproduce the selected-video endpoint

The original remote invocation took about three seconds end to end.
Fable's on-node repeat with deployed Jellyfin FFprobe 8.1.2 read 2,968
selected-video packets from a seek at 6300 seconds to demux EOF in 0.48
seconds. These measure different boundaries; neither is a cold-storage SLA.

The head probe found minimum PTS 0 and the expected initial unavailable DTS
for reordered H.264. The last eight packets were:

```text
PTS          DTS          duration
6423.292000  6423.250000  0.041000
6423.500000  6423.292000  0.041000
6423.417000  6423.333000  0.041000
6423.375000  6423.375000  0.041000
6423.458000  6423.417000  0.041000
6423.625000  6423.458000  0.041000
6423.542000  6423.500000  0.041000
6423.583000  6423.542000  0.041000
```

The endpoint is the maximum `PTS + duration`, 6423.666 seconds. The last
row is not the maximum because the output is in decode order. The resulting
span is 46 milliseconds below the container duration.

Two successful reads of the held source provide independent output checks,
not independent knowledge of an original movie length. A valid file cut at a
packet boundary can satisfy both. The proposal detects disagreement between
the source packet timeline and index output; it cannot recover absent bytes.

### 2.4 The terminal failure occurred during a reopen that never presented

Fable reconstructs session `s-c8070d…` as follows:

| UTC on 2026-09-18 | Observation |
|---|---|
| 20:01:48 | Reopen starts at requested film position 4,694,789 ms |
| 20:01:49 | Suspend: 32 s published, 30 s target, zero reported runway, reason `time` |
| 20:02:21 | Producer held; 32 s published, 20 s fetched; outer server progress idle 32,230 ms |
| 20:02:31 | AVFoundation -11866 and CoreMedia -12888; 43 s after load |

There is no `ttff` row for this session. Native telemetry reports
`AVPlayerWaitingToMinimizeStallsReason` with about 19.9 seconds buffered.
The 4,694,789 ms coordinate is the reopen target, not proof of playback.

The exact error is:

```text
AVFoundationErrorDomain -11866 · Playback Stopped
CoreMediaErrorDomain -12888 · Playlist File unchanged for longer than
1.5 * target duration
```

The local Apple SDK's `AVError.h` confirms that -11866 names
`AVErrorContentNotUpdated`. This corroborates a live-content update failure.

The review also reports two early-held sessions that did start: 19:49:19
had 84 seconds published, 13.8 seconds runway, and TTFF 4.3 seconds;
18:50:20 had 72 seconds published and TTFF 4.5 seconds. An early hold is
therefore a risk pattern, not a guarantee of failure.

### 2.5 Repeated holds exceed the update interval, with different client outcomes

Fable counted 98 time holds and 88 resumes. Among the paired holds:

| Measurement | Value |
|---|---:|
| Minimum / mean / maximum hold | 3.4 / 33.2 / 90 s |
| Holds exceeding 9 s, or 1.5 × 6 s | 86 of 88 |
| Holds exceeding 22.5 s, or 1.5 × 15 s | 77 of 88 |
| Typical segment duration | about 6 s; 608 s over 101 segments |

These counts do not prove every hold violated its exact observed target;
two paired holds were below 9 seconds. They establish chronic violations
consistent with the release arithmetic.

Session `s-761494…` survived nine 23–36 second holds on a roughly
60-second forward buffer without a visible symptom. Across 15 stalls Fable
found one `avplayer_item_failed`; other events were delivery stalls and
bounded reopens, not established terminal native failures.

Representative original snapshots follow. Progress idle uses the outer
`server` object consistently, not the slightly older nested
`client.server` observation.

| UTC | Recorded stall | Progress idle | Published / fetched |
|---|---:|---:|---:|
| 18:50:18 | 12.217 s | 71.786 s | 260 / 242 s |
| 19:00:55 | 20.829 s | 67.069 s | 638 / 608 s |
| 19:09:13 | 66.301 s | 67.254 s | 476 / 446 s |
| 19:14:35 | 70.619 s | 77.447 s | 332 / 314 s |
| 20:02:21 | 30.463 s | 32.230 s | 32 / 20 s |

The delivery stalls show 66–78 seconds without delivery and 18–36 seconds
published but unfetched. At 19:00:34 one response was abandoned after 253,952
of 5,106,684 bytes. This supports mutual waiting; it does not prove storage
failure or the same native error on every attempt.

Retention can alter a playlist during a producer hold. The general claim is
**no new media segment**, not byte-for-byte equality of every response.
The terminal session had fetched only 20 seconds, below retention pruning.

## 3. Root cause A — a known metadata gap becomes a terminal refusal

### 3.1 Why container duration was removed

The index pipe maps `0:v:0?`, drops audio/subtitles/chapters, and indexes the
selected video alone. A longer audio or subtitle track can extend the
container beyond the video's end. The prior sampled repair found 288 of 288
duration failures with usable video metadata matched the selected video
within two seconds. Restoring the container comparison would reopen that
false-shortfall defect; see the [prior RCA](CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md).

### 3.2 The missing fallback will not be discovered by automatic retry

[`automatically_retryable`](../../crates/plurx-core/src/content_analysis.rs)
automatically admits only budget and probe-timeout failures. Selected
source/process failures additionally require the transient allowlist.
`index_completion_unverified` is terminal: cluster storage writes a failed
row with `not_before_ms = i64::MAX`. Both
[SQLite](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs)
and [Hiqlite](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs)
must retain the same policy.

Installing a packet fallback does not rerun these rows. A reviewed
`force_rebuild` request for the exact current source identity is required.
W1 acceptance must prove failed → explicitly requeued → ready in a test store;
production requeue remains a separately executed rollout step.

This class was visible before enablement: the prior RCA recorded nine
no-duration outcomes. Fable's later 26-hour daemon-log cohort contains 62
outcomes over 52 distinct files, all reporting no trustworthy duration.
That is a log cohort, not a complete fleet inventory or permission to rebuild.

### 3.3 Completion still requires process, parser, source, and lease evidence

An expectation must name the exact mapped video and held source. Publication
requires clean FFmpeg exit, clean fragment-parser EOF, matching normalized
video coverage, and current source identity and lease at commit. Packet
provenance adds an expectation source; it does not replace these checks.

For packet expectations, compare coverage on both sides within the existing
two-second allowance. A lower bound alone would admit an under-measured
expectation. The current `covers` method remains the metadata-path lower
bound; add an explicit packet agreement check rather than changing every
metadata admission silently.

## 4. Root cause B — publication and fetch hysteresis form a mutual wait

### 4.1 Startup protection is tied to the wrong evidence

[`evaluate_flow`](../../crates/plurxd/src/transcode.rs) defines starting
using the unspent grant and published media below the 12-second floor.
`apply_ahead_window` also spends the grant when a playlist is visible and
the floor is reached. Both sites need correction.

With no reported runway, the target is 30 seconds. Publishing 32 seconds
permits a time hold; the suspended release threshold is 15 seconds. With
unchanged demand, production-ahead would have to fall by 17 seconds before
release. Fresh buffer reports can change the target; the trace shows that
they did not unblock this session before the terminal failure.

Fable's first-delivery or positive-buffer proposal improves the old predicate
but cannot establish startup: the failed client fetched 20 seconds without
rendering. The build contract uses accepted `RenderState::Rendering` plus
current-generation presentation progress, with bounded startup lifetime,
hard byte limits, and End authority preserved.

### 4.2 The reserve is also the amount the client must drain

At normal rate, away from the configured cap:

```text
target          = contiguous client runway + 30 seconds
release(target) = max(target - 30 seconds, target / 2, 1 second)
```

For targets at least 60 seconds, the release line is the runway itself.
Subtracting runway from production-ahead approximates published media beyond
the contiguous client buffer. The hold often releases only after the client
has fetched that reserve; Fable's healthy resume rows had physical ahead zero.
The exact relation depends on rounding, islands, rate, and the configured cap.

This makes a roughly 30-second publication gap structural in that regime.
It is not repaired by a faster poll or a larger reserve. The new scheduler
must replace this release rule for rolling time pacing and own every
subsequent hold, including evaluations between maintenance publications.

### 4.3 Playlist advancement needs its own owner

[`served_live_playlist`](../../crates/plurxd/src/transcode.rs) strips
EVENT, injects `EXT-X-START:TIME-OFFSET=0`, and passes the writer's ENDLIST
through. Zero offset starts at the oldest advertised segment; admission must
calculate runway from that position.

The 15-second repair loop is not a publication scheduler. The actor's
Advancing deadline is a producer-progress watchdog, disarmed while held.
It cannot enforce freshness during deliberate suspension.

The publication contract is anchored in
[RFC 8216 §6.2.1](https://www.rfc-editor.org/rfc/rfc8216#section-6.2.1):
unfinished playlists need new segments on a bounded cadence, and target
duration stays fixed. The original RCA overgeneralized the terminal error.
Native buffering can tolerate violations, but tolerance is not a server policy.

### 4.4 A growing target duration is a separate correctness defect

[`SessionDir::write_segment`](../../crates/plurxd/src/copyseg.rs) grows
the header from the longest segment so far. The configured 15-second cut
ceiling is not yet a strict bound: the cut decision sees the pending duration
before the next fragment, so a fragment can carry it past the ceiling.

A fixed integer target requires an enforced upper bound on actual output,
including tail handling. Fable's “ceiling plus one GOP” is not a finite
guarantee unless the GOP is bounded too. Strict pre-add accounting, oversized
fragment handling, and output checks belong in W2; never silently lengthen
an already-served target.

### 4.5 Recovery contains the symptom but does not establish one cause for all stalls

PR #803 preserves bounded reopen and same-delivery attribution. Here, delivery
stalls led to reopens and one reopen itself failed during startup. Retain
those distinctions in telemetry. The other historical delivery wedges remain
unattributed where an initiating native error was not retained.

## 5. Secondary defects — object grace and operator history

### 5.1 Segment removal has no serving grace

[`gc_expired_segments`](../../crates/plurxd/src/transcode.rs) renames
served objects, marks them pruned, and queues unlink in one pass. Clients
holding the previous playlist can lose a previously valid URI immediately.
No retained segment 404 identifies this as the incident trigger.

The correction requires separate advertised, grace-servable, and deleted
states. Update `first_retained_index`, `prunable`, `server_ready`,
takeover baseline, and `segment_was_pruned`, plus request authorization and
accounting. Grace bytes are real disk use.

### 5.2 Typed outcome recording fails on the node-local store

Fable reports 64 “no such table: settings” errors in 26 hours and no local
outcome row for 6109. Source inspection confirms
[`record_typed_outcome`](../../crates/plurx-core/src/store/fragindex.rs)
queries `settings` inside its local transaction. The
[telemetry store](../../crates/plurx-core/src/store/telemetry.rs) calls it
from a different database role.

The implementation must pass resolved retry policy to this helper through
the store boundary, rather than create a shadow settings table or swallow
the transaction error. Preserve authoritative cluster outcome and repair
future local history writes. Do not fabricate historical rows from log text.

### 5.3 A terminal index is reported as pending

[`vodserve`](../../crates/plurxd/src/vodserve.rs) uses
`vod_index_pending` where the index may be terminal. Add accurate diagnostic
state without accidentally disabling fallback in clients that recognize the
existing reason. Any changed wire reason needs all consumer mappings and
mixed-version tests in W4.

## 6. Fix A — bounded packet evidence, exact failure semantics

Collect every selected-video metadata candidate before resolution. Check the
range of all candidates, not only their distance from candidate one. Prefer
the existing metadata order when they agree; use packet evidence for absence
or conflict. Preserve existing positive artifacts.

Use held-source packet reads with integer PTS/duration and checked signed
`i128` rational arithmetic for origin and end; the old unsigned helpers
cannot parse negative timestamps. Only the positive final span becomes a
`VideoCompletionExpectation`. This restriction applies to completion, not
every probe in the repository.

Read a bounded prefix for origin and a tail from the container seek hint
through demux EOF. Seek is approximate; see
[FFprobe interval semantics](https://ffmpeg.org/ffprobe.html).
A capped interval is not EOF proof. Minimal output, aggregate byte/time
limits, bounded backward widening, cancellation, and descriptor reset are
mandatory. Unsupported timestamp shapes remain unverified.

| Outcome | Policy |
|---|---|
| Packet/index agreement within 2 s and all existing checks pass | Publish with PacketTimeline provenance |
| Output falls short by more than 2 s | `index_video_shortfall` |
| Output exceeds packet span by more than 2 s | `index_completion_unverified`, packet/index disagreement |
| Probe timeout / whole-job budget exhaustion | Existing retryable `index_probe_timeout` / `index_budget_exceeded` |
| Nonzero FFprobe exit | `index_process_failed`, terminal unless an existing explicit transient rule applies |
| Typed source I/O | `index_source_io`; retry only with the existing transient allowlist |
| Output cap, absent fields, unsupported origin, unresolved conflict | Terminal `index_completion_unverified` |
| Source replacement or lost lease | Existing supersession/fencing path; never publish |

An older binary rejects unknown provenance while decoding the whole
diagnostic, yielding null rather than a crash. Forward-compatible diagnostic
reading must land before packet provenance is emitted. Rollback must target
that compatible baseline; compatibility cannot be retrofitted into older
binaries by documentation.

## 7. Fix B — startup admission and an actor-owned publication schedule

### 7.1 Start with a bounded startup correction

W0.5 changes both the grant predicate and latch. Server publication or a
download alone cannot end startup protection. Current accepted render and
position evidence must prove presentation began; old reports, seeks, and
replacement attempts cannot spend or renew the wrong grant. A finite startup
deadline prevents an abandoned client from producing indefinitely.

This is an independently testable mitigation. It does not claim to fix
steady-state publication or Pause, and needs physical startup validation before
any claim that it closes the observed terminal case.

### 7.2 Freeze the target and publish snapshots deliberately

The actor owns a fixed target T, an immutable externally available snapshot,
and the next publication deadline. Separate completed segment production from
making a new playlist available. Multiple short segments may need a single
publication batch; one six-second segment every sixteen seconds is insufficient
for 1× playback even if the sixteen-second target's deadline is met.

Set the first-response inventory from the zero-offset start, supported rate,
network margin, and next publication interval. Keep the unfinished advertised
window at least three target durations. Finished short films can publish with
ENDLIST. Cover copy and other rolling writers; no writer may bypass the served
snapshot contract with a changing target or early raw-manifest exposure.

### 7.3 Replace old time hysteresis with bounded production and publication cycles

The schedule admits new segment-bearing versions between 0.5T and 1.5T,
aiming near T. It produces enough media per cycle for the client's rate,
with bounded staged lead. Every later flow evaluation obeys the scheduler;
the old 30-second release threshold no longer controls rolling time holds.

Retain the measured active-production rate across holds, with age and attempt
fencing. `recent_speed_survives_a_hold` proves current EWMA preservation.
Unknown throughput grants no speculative hold; the configured read rate is a
pacing ceiling, not a throughput guarantee. Combine conservative measured
timing and hard deadlines; test both short runs and long GOPs.

The six-second example holds about three seconds and runs about three at 2×
only when target, segment duration, and publication batch permit it.
Variable segments and larger targets require batched arithmetic, specified in
the [implementation handoff](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md).

### 7.4 Demand Hold is in scope; hard caps still win

Use the same publication clock during intentional Pause for a proposed grace
of 180 seconds from the first accepted Hold. Repeated Hold or reload requests
do not renew it. At expiry, retire the presentation; Resume keeps the saved
film position and goes through the existing fenced replacement path.

This adopts Fable's proposed value for implementation/testing; Paul has not
separately approved it as a deployed product setting. Keep it isolated and
explicit in release notes. Lease expiry, End, failures, and hard storage limits
can retire sooner. Do not wait indefinitely for a pause-specific follow-up.

Hard caps cannot be bypassed to meet a deadline. Reserve space for committed
URI grace and the next bounded batch before writing. If unavailable, stop
production and signal a controlled unavailable presentation. Count that as a
failed playback/recovery outcome, not successful deadline compliance.

### 7.5 Retirement still owes previously published objects

Retirement is not permission to delete served media immediately. RFC 8216's
whole-presentation removal rule and per-segment grace remain applicable.
Retain authenticated read-only access to promised objects, including init
data, while preventing new production or old callbacks from controlling a
successor. Explicit user/security revocation retains its existing priority.

The old plan's “retire under a hard cap” needs this admission reservation:
retirement cannot free bytes already promised to an old playlist immediately.

## 8. Fix C — separate visibility, serving, and disk lifetime

A removed URI remains servable for its segment duration plus the longest
distributed playlist duration that contained it. Use the actual served
window, not the full writer history. Removal time and grace are tied to the
snapshot becoming available, not to an arbitrary scan.

The roughly 180-second retained window plus ahead means grace can add roughly
another window of bytes at steady bitrate. This is a planning estimate; record
measured peak scratch for variable bitrate. It increases global-cap pressure;
the existing per-session ahead-byte calculation is not the same as total
session disk use. Audit both, rather than claiming both caps already charge
identical totals.

Keep the advertised window unchanged initially. If capacity cannot cover
promised grace, make the capacity refusal explicit. Shortening the advertised
window can reduce future grace, but needs its own rewind/seek acceptance and
cannot shorten promises already made. Do not silently raise the 2 GiB ahead
or 8 GiB global defaults.

## 9. Alternatives and platform consequences

| Shortcut | Why it fails |
|---|---|
| Restore container duration | Reopens longer-audio false failures |
| Clean index EOF without independent expectation | Cannot detect incomplete output against a source endpoint |
| Tail window cap treated as clean EOF | Admits an under-read endpoint |
| First download proves startup | The failed reopen already downloaded 20 s |
| Increase the reserve | Also increases drain-based hold duration |
| Pulse once, then restore old hysteresis | Later flow evaluations recreate the long hold |
| Change comments, MEDIA-SEQUENCE, or cache headers | Does not provide newly playable media |
| Add ENDLIST when held | Falsely ends a partial film |
| Mutable target or an unbounded “extra GOP” allowance | Cannot guarantee every later segment fits the original header |
| Assume configured 2× equals measured throughput | Slow source or encoder misses its promised deadline |
| Retire and immediately unlink | Breaks already-issued object promises |

Apple's exact native error is established for the terminal reopen. Fable
reports the vendored hls.js reloads unchanged playlists faster without the same
fatal condition; web still needs drain/recovery tests. Android is pinned to
Media3 1.10.1. Its
[playlist tracker](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer_hls/src/main/java/androidx/media3/exoplayer/hls/playlist/DefaultHlsPlaylistTracker.java)
uses 3.5T for detecting an unchanged playlist, correcting Fable's 3T estimate.
That is tracker behavior, not a measured device-failure deadline or a claim
that every detected exception terminally fails playback.

## 10. Review decisions — disposition and remaining release choice

| ID | Build ruling |
|---|---|
| R1 | PacketTimeline fallback plus two-sided packet/index agreement and explicit requeue test |
| R2 | Container duration is a seek hint only |
| R3 | Actor owns served-snapshot publication and deadline |
| R4 | Fixed covering target; strict segment bound and oldest-segment startup runway |
| R5 | Replace time release policy; all evaluations respect bounded publication cycles |
| R6 | Hard caps cause controlled retirement with reserved object grace |
| R7 | Implement/test 180 s pause grace; label it a proposed deployment default |
| R8 | URI grace ships with the rolling repair |
| R9 | Exact terminal cohort requeue is necessary; prove it in tests and preview production targets |
| R10 | Build W0.5 startup mitigation first as a separate review unit |
| R11 | Include Demand Hold in the publication clock from the start |

## 11. Work packages — the handoff is the execution contract

The [implementation plan](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md)
defines W0, W0.5, W1a/W1b/W1c, W2a/W2b, W3, W4, and W5. It includes the local
history and misleading-pending side defects as bounded tasks because they
otherwise obscure duration acceptance.

Use one effort branch: these packages share files. W0.5 is first and separately
reviewable within that effort. “Ship first” in the review is a priority
recommendation, not an exception to the contributor's integration and
qualification rules or an instruction to deploy an unqualified effort.

## 12. Verification — two routes, startup, pause, and all object lifetimes

Required evidence includes:

- a no-duration fixture that first reproduces the existing refusal;
- terminal → force request → ready, including source change and deduplication;
- signed packet arithmetic, candidate disagreement, actual EOF, and both sides
  of coverage comparison;
- the 32 s published / 20 s fetched / zero presentation startup trace;
- a moving client, a fetch-stopped client, and a paused client over many cycles;
- fixed-target batches with 6 s, 15–16 s, short-first, and EOF-tail shapes;
- old producer events and response snapshots racing replacement;
- short run / hold cycles without watchdog starvation or false failure;
- old playlist and whole-retirement URI requests through the full promise;
- explicit capacity refusal without deleting promised media; and
- physical iPad, Android, and web evidence with method and version recorded.

Synthetic clocks prove policy. Real FFmpeg/FFprobe tests prove process and
timestamp assumptions. Device tests prove integration. A green immutable
VOD test does not accept the forced rolling route.

## 13. Rollout and rollback — repair existing rows deliberately

Qualify the exact effort tree before release. The rollout sequence is compatible
diagnostics/history, startup mitigation, rolling publication plus grace,
packet-fallback enablement, and an explicitly authorized exact-cohort requeue.
Packages may be released together after qualification; separate releases each
need their own current evidence.

A requeue preview must include source identity, current terminal code, policy
version, and whether a valid positive artifact already exists. Do not use log
counts as mutation targets. Preserve history and positive indexes.

Disable packet fallback for new work without deleting accepted artifacts.
Rollback only to a reader that preserves diagnostics with unknown provenance.
A pre-compatibility binary yields null diagnostics; that is a known rollback
limitation. Disable new rolling admissions or drain/retire with object grace
rather than revert active playlists to indefinite freezes.

## 14. Non-goals — keep source correctness and playback boundaries intact

Do not restore container duration as video authority, increase the two-second
allowance, lower quality to hide delivery failure, rewrite private media,
make recovery unbounded, rebuild successful indexes, or delete promised objects
to make a quota test pass. Evidence uses neutral names. This document and the
handoff do not themselves deploy, requeue production jobs, or launch Sol.

## 15. Fable review — every finding has a recorded disposition

The supplied review was titled “Review — MKV duration and Apple sliding HLS
RCA” and pinned to `395ce591`. Its live observations remain attributed to
Fable, while source conclusions below were checked during this revision.

| Finding | Disposition and implementation consequence |
|---|---|
| 1 — terminal startup | Accepted causal correction; W0.5 includes both predicate and latch. First download / positive buffer is insufficient; require presentation evidence and bounded lifetime. |
| 2 — chronic release rule | Accepted with scope: target ≥60, away from cap, contiguous runway. 86/88 rather than literally every paired hold exceeded 9 s. W2 replaces release, not a one-off pulse. |
| 3 — tolerated holds | Accepted; distinguish delivery stalls, reopens, and the one terminal item. SDK verifies -11866 name. |
| 4 — terminal rows and cohort | Accepted; no automatic retries. W1c tests force rebuild; log cohort was known, not newly discovered at review. Probe wall time distinguishes local execution and remote invocation. |
| 5 — history and pending labels | Accepted source-supported side defects; W1a repairs policy plumbing, W4 repairs operator state with compatibility. |
| 6a — one-sided coverage | Accepted; packet-only symmetric agreement, unchanged metadata policy. |
| 6b–6d — arithmetic, failures, caps | Accepted; signed checked math, exact retry matrix, minimal output and aggregate bounds, no capped-tail success. |
| 6e–6h — compatibility, CSV, fixture, candidates | Accepted; diagnostic reader first, completion-only integer rule, missing-duration fixture, all-candidate range check. |
| 7a–7b — Hold and variable segments | Accepted; Pause is in scope; batch publication handles variable durations. |
| 7c — recent rate becomes None after holds | Corrected: current code preserves EWMA; `recent_speed_survives_a_hold` explicitly tests this. Do not replace unknown throughput with a presumed 2× guarantee. |
| 7d — oldest start | Accepted; initial inventory and runway use injected zero offset. |
| 7e — advancing watchdog | Test required; rearming is real, but a hold disarms a live deadline. The claimed per-cycle false firing is not established; test false failure and indefinite budget renewal. |
| 7f — byte equality | Accepted; use new segment advancement, not raw-byte identity. |
| 8 — grace plumbing/cost | Accepted; audit all five consumers plus HTTP authority and retirement. Rough doubling is an estimate; ahead bytes and total live bytes are distinct. |
| 9 — platforms | Accepted distinction; corrected Media3 default to 3.5T using pinned source. W4 corrects web comment and exercises actual clients. |
| 10 — names | Accepted; neutral film/node names, no private source path. |
| 11 — details | Accepted; dated builds, subtitle duration clause, outer-server timing, writer ENDLIST pass-through, Demand hold included. |
| R4 cutting prescription | Qualified: including an unbounded crossing GOP cannot prove a fixed bound. W2a must enforce the bound or return a typed unsupported result before exposing oversized output. |
| R7 pause choice | Plan specifies an isolated, tested 180 s default; implementation is still pending and the deployment choice remains visible for Paul. |
| R10/R11 | Accepted priority and scope; contributor effort rules still apply. |

A further integration correction emerged while reconciling the review:
whole-presentation retirement must preserve promised media too. The handoff
makes quota reservation and retirement grace part of W3, and publication
batching part of W2b, so the proposed fixes cannot evade one contract by
breaking the next.
