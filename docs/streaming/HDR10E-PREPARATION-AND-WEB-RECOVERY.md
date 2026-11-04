# Enhanced playback preparation — supply, observation and resume repairs

**Status:** open · **Updated:** 2026-11-03 · **Owner:** Codex ·
**Branch:** `codex/m6-hdr10e-preparation`

Companion to [playback](../PLAYBACK.md) (delivery and progress),
[the DV helpers](DV_PROCESSING_TOOLS.md) (window validation), and
[the cache budget](PLAYBACK-CACHE-BUDGET.md) (physical capacity).
This ledger distinguishes three faults reported during one 4K P7 playback
investigation. It does not declare physical acceptance from unit tests.

## Current-main source custody changes require new qualification

Main `71a776b3a` landed the separate startup/source-custody repair in #1366.
Production DV windows now read through the held-source service, with exact
capability and Range validation and settlement after actual child reap. The
32-window measurements below qualify the earlier direct-descriptor graph;
they do not qualify this integration.

The synchronous source-service baseline retained every check and original
surround audio. Its 32 windows contained 1,536 frames over 64 seconds, but
subsequent waits averaged 2,204.6 ms (median 1,795 ms; maximum 3,485 ms).
Thirteen consecutive slow windows accumulated 9,330 ms of readiness debt;
final debt was 6,342 ms. Local bodies took at most 6 ms. This graph still falls
behind the two-second media cadence; passing media checks is not throughput
acceptance.

The reader adaptation calls the authoritative adapter for read, seek and
charging. Local reads spend returned bytes; remote reads spend the entire
requested Range before transfer, including partial, failed and discarded
lookahead. The worker never charges twice or refunds a seek. Its own four-slot
queue adds 4 MiB and one explicit 256 KiB stack to existing service/socket
buffers; the extra CPU worker remains admitted. A latched nonblocking cancel
pipe wakes a policy-paused socket request without cross-thread socket closure
or a wall timer. Only the I/O owner closes its socket. Reset and destruction
follow worker join; source-service settlement still follows actual child reap.
Seven pinned native metadata/reader controls pass, including real HTTP
partial-body cancellation, positive-budget seek/restart without refund,
queued invalid-header errors, exact cap exhaustion, and cancellation priority
after interruption. Address/undefined-behavior controls pass ten local reader
runs plus the remote controls; no ThreadSanitizer execution is claimed.

The complete same-window comparison ran synchronous → reader → reader →
synchronous, each with 32 original-audio publication windows and all checks:

| Source adapter | Create (ms) | Mean subsequent wait (ms) | Maximum wait (ms) |
|---|---:|---:|---:|
| Synchronous, first | 2,264 | 2,204.6 | 3,485 |
| Reader, first | 2,886 | 1,612.6 | 1,756 |
| Reader, repeat | 2,017 | 1,580.3 | 1,668 |
| Synchronous, repeat | 3,664 | 1,912.3 | 2,550 |

Both reader runs prepared every subsequent segment ahead of its two-second
media duration: 62 waits averaged 1,596.5 ms, with a 1,756 ms maximum. Across
both synchronous runs the mean was 2,058.4 ms, a measured 22.4% reduction in
this balanced control. The warmer final synchronous run still had a 2,550 ms
window and a 407 ms cumulative readiness deficit. Reader runs accumulated no
positive subsequent readiness deficit. Initial create time is reported
separately and is not uniformly faster.

Each run independently decoded 1,536 frames spanning 63.9999996667 seconds,
with joined AAC/video timing, Main10/PQ/BT.2020/TV signalling and every
source, custody, RPU, FEL, header, receipt and publication check retained.
Segment byte lengths matched across all four runs; this is not a claim of
content-hash or calibrated display equivalence. Local body reads took at most
7 ms. These are private SQLite/manual VOD controls through the real source
service and local published-file reads; viewer-facing HTTP, replicated-store
and live browser acceptance remain separate.

The new base also upgrades managed schema v3 to v4 and appends SQLite
migrations 122–123. An old-image rollback is not valid after activation:
the old binary cannot reopen that schema. During qualification a concurrent
rollout upgraded the baseline on a voter and a learner. Read-only, WAL-visible
schema queries then confirmed managed v4 on all four replicas. This task has
not restarted a service or activated a schema. Recovery must use a v4-aware
image; remaining old voters must not be restarted with an old binary on the
assumption that image tags alone provide rollback. Candidate deployment still
requires coordination with that rollout and a qualified compatible recovery
image.

The separate November 2 callback RCA does not establish the missing JavaScript
ownership state in this November 3 Safari incident. Its incoming subscription
renewal follows source/seek lifecycle events; ordinary active playback has no
age-triggered renewal. Independent progress may defer destructive recovery,
but it does not manufacture presentation receipts.

## Evidence identifies three different failures

The deployed source inspected was `7ea74370b`; the repair starts from
`bd213c819`. DV execution and generation matched the deployed source.
Private incident captures remain outside the repository; recorded summaries
contain numeric timings rather than account or session identifiers.

| Observation | Established conclusion | Limit |
|---|---|---|
| Chrome on lab6: 37 captured two-second media requests; median first byte 1,997.9 ms, median body transfer 57 ms; 27 requests exceeded two seconds. Same browser and processing through media1: 14 requests, medians 1,510.3/46.3 ms; none exceeded two seconds. | The dominant observed wait preceded media delivery; the client repeatedly exhausted its runway. | Different movie windows and cache conditions prevent assigning the entire difference to one stage. |
| Local installed renderer/VA-API encoder diagnostic: two seconds of 4K FEL/RPU video took about 1.2 seconds. The complete daemon window harness retained raw-RPU, encoded/header, plan and receipt checks. Its validation stages took tens of milliseconds. | The renderer-only diagnostic omitted production work. Removing validation is neither justified nor necessary. | Unoptimized test diagnostics and scene variation prevent treating these figures as a sustained production rate. |
| Safari first stopped when its native audio track ran dry; video decoding remained healthy. After reconnect, native logs reported about 24 displayed frames/s and an advancing timebase while the application declared a persistent stall and eventually stopped playback. | Initial audio starvation and subsequent false application recovery are distinct. | The logs do not identify the JavaScript callback, clock or ownership divergence. |
| A partial rewatch retained its saved position and `watched=true`; the UI required `!watched` to resume it. | Historical watched state incorrectly suppressed a valid current position. | No zero-position database overwrite was established. |

The later failed-seek trace captured the outgoing attachment's origin once.
Its zero origin was not evidence that the replacement used zero: the actual
attachment calculation mapped a 4,319.584-second film target to a 4,318.792-second
origin plus 0.792-second element position. No speculative origin patch is made.

## Preparation keeps every publication check

Encoder input discovery, AAC preroll, priming, offsets, filters and duration
remain unchanged. A NUT-input discovery experiment showed less than 1% median
gain in a balanced twelve-run comparison; that change was dropped.

After both children have completed and been reaped, optional cache inventory
runs beside existing verification. It uses the same registered global cache
budget. Busy operations, an unknown ceiling or an occupied inventory slot
cause warming to skip. Optional waiting is bounded to 250 ms; warming never
grants capacity or becomes a DV-validation refusal.

Publication still performs its ordinary reservation. A warmed inventory can
include a temporary probe copy removed before publication. If that sample
would cause capacity or free-space pressure, reservation forces a current
inventory before eviction or refusal, then recomputes device/allocation and
retains dirty-revision reconciliation and the existing total deadline.

The following remain required before publication:

- Held source identity, source clock, raw RPU association and FEL evidence.
- Helper completion, exact encoder success and owned-job retirement.
- Picture count, raster, cadence, sample timing and encoded payload checks.
- The independent held-output header/audio probe.
- Completed-plan validation, processing receipt and source/backend fences.
- Ordinary disk reservation, free-space floor and publication ownership.

Debug timings use one entry identity. `finalize.*` timings are nested within
`finalize_and_backend`; do not sum both. Request/admission and publication
waiting remain separate. Physical controls can select up to 32 explicit
entries; an explicit out-of-range request fails instead of disappearing.
Cache comparisons require complete ABBA blocks of the same entry and report
actual completed warming separately from permission to warm. A read-only
control measured the real Rust inventory at 91–94 ms for 25.84 GB allocated;
this measures inventory, not complete file or HTTP publication.

One same-entry ABBA control retained every check and measured preparation plus
reservation at 1,568/1,494 ms without overlap and 1,475/1,449 ms with overlap
(medians 1,531/1,462 ms). The two warmed scans completed; reservation then took
0 ms instead of 94–96 ms. All samples retained 48 FEL and receipt frames.
Renderer time also varied (1,317–1,386 ms), and expensive test-only luma
diagnostics separated samples. This proves a bounded inventory improvement,
not sustained production throughput, HTTP delivery or smooth playback.
The earlier HDR10-E fixture did not inject the detected Mesa driver policy.
Those ABBA figures qualify the inventory overlap for that controlled recipe;
they are not production-policy throughput evidence. The physical fixture now
detects the encoder and preserves its actual policy for every destination.
Transferred executables and generated media were automatically removed.

## Full publication timing still requires attribution

An isolated normal VOD session now exercises the registry, retained driver,
private verification, publication and four consecutive segment reads. It stops
and settles every producer before independently decoding the served segments
and checking joined video/AAC cadence, raster and HDR signalling. The actual
Mesa policy control passed with 192 frames spanning eight seconds. This is an
unoptimized Rust test with a private SQLite store, manual session selection
and local file reads; it does not measure HTTP or replicated-store delivery.

At entries 1122–1125, one policy-correct run measured create at 1,510 ms and
subsequent segment waits at 2,535/2,325/2,382 ms; bodies took 3 ms. Another run
measured 1,395/2,431/2,343 ms. Child-lifetime observation placed the long waits
inside live renderer/encoder execution, rather than a fixed successful
retirement grace period. Similar child CPU totals across fast and slow windows
do not identify whether source I/O, pipe blocking or GPU waiting caused them.
The in-flight prefetch after stopping the session is excluded from throughput
claims. Standalone raw-video/luma diagnostics precede these controls and may
change their environment; a separate publication-only mode avoids that load.
Its first four windows had useful headroom, but a longer 32-window run still
fell behind: 2,219 ms median and 3,055 ms maximum wait for two-second segments.
All 1,536 served frames spanned 64 seconds with valid joined audio/video cadence.
Short controls therefore cannot establish sustained preparation throughput.

Several native controls were rejected, with quality policy unchanged:

| Experiment | Balanced median observation | Disposition |
|---|---|---|
| NUT direct AVIO writes | 1,276 versus 1,388 ms | Reverted; slower despite lower CPU time. |
| 64 KiB versus 1 MiB raw-video pipe | 1,308 versus 1,306 ms | No meaningful gain. |
| VA-API queue depth 1 versus 4 | 1,258 versus 1,279 ms | No gain; existing bounded depth remains required. |
| Mesa shader cache disabled versus enabled | 1,270 versus 1,254 ms | Insufficient gain; unmanaged driver caches remain disabled. |

Each comparison retained source fencing and frame/FEL evidence; separate raw
pixel/PTS controls matched. These native controls omit normal daemon admission
and publication, so they do not replace the complete validation harness.
The common launcher disables driver caches even when the renderer supplies a
private shader-directory path. Enabling those writes would require enforceable
capacity admission; the path and Mesa's approximate size setting are not enough.

The retained durable request was joined internally to its incident telemetry.
It resolves eight source audio channels to AAC 5.1, six channels at 320 kbps,
48 kHz. The initial manual control used stereo at 160 kbps. That bitrate
difference predicts 40,000 extra bytes over two seconds; the corrected canonical
audio claim produced the original three captured segment lengths exactly:
4,766,965, 4,545,736 and 4,735,176 bytes. This resolves the size discrepancy
without an AAC-preroll defect. The corrected control passed normal publication,
decode and eight-second joined cadence checks. Equal lengths are not retained
content hashes, and manual admission does not restore an old Auto owner's lease.

An isolated source-inode read interposer then measured the sustained slow
control. Renderer read syscalls took a median 984 ms per window; encoder reads
took 531 ms. Individual reads reached 200–320 ms. The children run concurrently,
so their read durations must not be added to derive wall time. Instrumentation
adds an `fstat` before reads and excludes that overhead from its read duration;
uninstrumented sustained waits were similarly slow. Warm same-window reads took
only milliseconds. This identifies source-read waiting as a substantial cost in
the reproduced control, not an incident-era trace of NFS transport specifically.

Both nodes' backing-device read-ahead settings were 128 KiB. A helper-only
sequential-access hint showed no useful gain across balanced, disjoint first-touch
blocks and is not a product change. A 1 MiB input buffer gained about 7% in one
balanced comparison, while 4 MiB was slower. Neither buffer change is retained;
the AVIO input buffer remains 64 KiB. Driver-cache enablement, kernel prefetch
hints and global mount tuning are not part of this candidate.

## The owned reader overlaps source waiting with rendering

After the final demux seek, one helper-owned reader fills four 1 MiB slots while
the existing decoder/render/encode pipeline consumes them. The four-slot bound
includes an in-flight read. The reader has an explicit 256 KiB thread stack;
the existing AVIO buffer, thread guard and TLS are additional memory. Admission
reserves one extra CPU worker: the GPU FEL/audio graph now debits ten threads
rather than nine. There is no new setting or observer retry.

Every completed descriptor read spends the same 128 MiB allowance, including
queued, discarded and in-flight data. Seeks never refund those bytes. Queued
bytes are delivered before EOF, errors or a terminal budget refusal. The reader
tracks bytes delivered to AVIO separately from AVIO's own buffered consumer
position; seek pauses and joins it, restores the callback cursor, applies the
seek and restarts. Completion stops it before writer/GPU cleanup and joins it
before closing the source or reporting success. A blocked filesystem read has
no promised completion time; existing daemon deadlines and actual-reap ownership
remain authoritative. The new header participates in both helper source identities.

The native first-touch comparison retained original six-channel audio and
matched raw pixels, PTS and encoded bytes on its same-window fidelity controls.
Instrumented baseline/reader median wall times were 1,995/1,712 ms. A separate
uninstrumented comparison measured 1,436/1,362 ms; its source/cache conditions
were different. Disjoint windows and source variation prevent treating these
as paired same-scene gains. Additional application reads may warm subsequent
windows; they remain counted, and no claim equates them to extra NFS wire bytes.

The standard rebuilt bundle and matching daemon then passed 32 consecutive
normal VOD publication windows with the recovered AAC 5.1/320 kbps recipe.
Create took 2,143 ms; the next 31 two-second segments had median/mean waits of
1,522/1,518 ms and a maximum of 1,646 ms, with body reads at most 5 ms. All 1,536
frames spanned 64 seconds with valid HDR metadata and joined video/AAC cadence.
With the first segment prepared, every subsequent local object became ready
before its simulated two-second playback boundary. These are isolated manual
VOD/local-read results, not HTTP, replicated-store or browser acceptance.

## Native observation remains a root-cause investigation

A proposed seek-completion renewal could discard the only queued paused
landing callback and was removed. A narrower once-per-attachment active renewal
passed focused tests and Astra's code review, but the owner rejected it as a
symptom workaround without an identified incident cause. It is also removed.

No observer-retry or seek-lifecycle change remains in this candidate. The
historical native player was mapped to the visible current element; its
continued display was not a hidden prepared stream. The callback, sampled
JavaScript clock and ownership state at the divergence were not captured.
Native decoding statistics alone do not establish accepted application receipts.

Five isolated Safari cases using generated SDR HEVC/AAC HLS reproduced no
stuck observer through load, reload, seek/pause or ownership rejection. These
are negative evidence only: native registration could be deferred by readiness,
reload totals included predecessor callbacks, and the fixture did not reproduce
the movie's growing Dolby Vision/E-AC-3 stream. The fixture server, test tab and
generated media were removed immediately after the run; compact receipts remain.

A separate source counterexample loses observation when a full replacement
play fails while preserving a started predecessor: the open invalidates its
attachment, an outstanding callback retires, and refusal restores `PLAYER`
without coherent attachment/observer ownership. Ordinary native recovery and
the recorded cold close/reopen and later seek do not take that restoration
path. It remains a separate defect, not an explanation or patch for this stall.

## A rewatch position is independent of watched history

Local and Shared launches use the same resume calculation. A valid position
above three seconds and below 95% of known duration resumes even when watched
history remains true. Completed/unstarted positions retain start-over behavior;
unknown-duration watched records retain conservative behavior. No saved watch
state or watched flag is rewritten by this repair.

## Review and acceptance

Astra independently reviewed causal claims and changes. Its rejected
seek-completion renewal could discard the only queued paused landing callback;
that design was removed. The active-observer workaround was subsequently
rejected by the owner despite passing its narrow code review. Astra also required consistent timing identities,
rejection of invalid explicit physical samples, bounded optional cache work,
pressure reconciliation for removed probe files, and a timeout/cancellation
test for inventory ownership.

Focused resume regressions have passed, including Shared launches (22 tests).
The cache suite passed 27 tests; its read-only physical timing control is
explicitly ignored by ordinary runs. Both source-service reader controls and the balanced synchronous comparison
passed with all validation intact. Native reader controls cover cursors, unread
AVIO data, seeks, short reads, EOF/errors, exact budget exhaustion, discarded
reads, start refusal and owned shutdown. Seven native metadata/reader tests and
12 focused Rust resource/lifecycle tests and 29 runtime metadata/receipt tests
passed. Address/undefined-behavior
sanitizer controls passed ten runs with a fixed executable mapping; ThreadSanitizer
could not start on the test kernel, so it supplies no race-check evidence.

The rejected observer patch's tests do not establish incident closure. The
64-second publication result establishes server preparation headroom under its
recorded conditions. No deployment or live browser acceptance is recorded yet;
final compiler/CLI gates and a matching Safari JavaScript capture remain separate.
