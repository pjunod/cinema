# Cold playback retest — source starvation and the 720p bootstrap chain

**Status:** open; captured mechanisms established, fixes proposed,
continuity and historical rate attribution still incomplete · **Written:** 2026-11-02 EST · **Runtime:** `8a212be7021f` ·
**Scope:** post-deployment English AC-3 retries of The Craft, file 6964.

Companion to [the canonical startup RCA/proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md).
This adds the afternoon production traces and changes the repair scope. The
independent M2 audio fix is deployed and accepts English AC-3. It does not
repair source scheduling, rolling publication, or continuous-quality startup.
The user confirmed the external movie transfers had finished before these
retries; they do not explain these captures. Infrastructure names below use
the repository's mirror-safe aliases.

The measured original-copy replay becomes **input-starved on the shared NAS
source disk**, with a second reader of the same movie and other cluster
source traffic. The rolling publisher also withholds a playlist until a
48-second media runway is available. The 720p route adds a separate dependency:
it starts two video renditions and an audio rendition and waits for every
child's verified initialization before returning its family description.
A captured pending response then causes the browser to release the entire
session, stopping the producers it was waiting for.

These are distinct mechanisms. The historical `0.644x` warning is a recent
progress estimate, not a measurement identifying its cause. The instrumented
replay proves source starvation in that replay; it cannot assign every second
of the earlier failure, including its overlapping Raft timeouts. The first
720p attempt's initial delay and later stalls are retained separately from
the later, conclusively captured family-response failure.

## Captures — keep each attempt and its clock separate

All timestamps are UTC on November 2 (subtract five hours for EST). Client
TTFF is its own monotonic duration. Server phase elapsed fields have their
own origins and are not interchangeable with click-to-frame duration.

| Attempt | Observations | What it establishes |
|---|---|---|
| 18:53:04.933, manual 720p + AC-3 | `vod_create` 444 ms; overall create accounting 678 ms; first manifest dispatch 18:53:14.186; client TTFF **14,560 ms**; subsequent buffer stalls | Session creation succeeds quickly but bytes/presentation do not. No family 503 or deletion was captured here. |
| 18:55:05.012, fresh Original + AC-3, worker `media1` | Exact preparation unavailable; video **copy** and audio **copy**; first complete segment 169 ms after producer/create baseline; writer gate at 3.205 s; client exhausts startup at 41.162 s; retirement reports `0.644x` | Audio conversion and encoder throughput cannot explain this attempt. The publisher never delivered the initial playlist before the deadline. |
| 19:13:54.346, instrumented Original, worker `lab4` | First segment 260 ms; writer gate at 1.742 s; first snapshot 19:14:07.130 with 50.842 s media, policy `Conservative`, bootstrap 48,000 ms; later producer inventory grows below real time | Producing an early segment is insufficient under the current publication policy. Source reads become the dominant wait in the sampled slowdown. |
| 19:31:04.417, instrumented manual 720p, worker `media1` | Session POST 1,309 ms; family GET waits 8,051 ms then returns 503 `segment_pending`; client immediately DELETEs session, receives 204; no first frame | A pending bootstrap response is treated as fatal and destroys the work required to satisfy it. |

The 18:54 optional switch from 720p to Original declined while retaining the
existing session. Its precise refusal was not captured; this report does not
attribute it to the already-fixed audio identity bug.

## Original copy — the NAS source path, not encoding, limits measured supply

The controlled Original replay ran on `lab4`, despite the browser connecting
to `media1`. Routing origin does not pin the worker. OS samples followed the
actual FFmpeg PID 944615 and its source FD 4, rather than assuming the entry
node owned production. The producer copied both video and selected AC-3.

After its initial burst, completed writer inventory grew at **0.919 media
seconds per wall second** over 19:14:21–54 and **0.905x** over
19:14:55–19:15:30. These are inventory deltas, distinct from the daemon's
recent progress EWMA. FFmpeg used about 0.02 CPU cores in those windows and
its input thread was sampled in `folio_wait_bit_common` in 32/34 and 30/36
observations. Source input fell from 68.20 MiB/s in the initial burst to
15.65 and 13.27 MiB/s in those later windows.

A bounded source-read trace at 19:14:15.363–20.040 captured 323 completed
reads of the source FD: 75.811 MiB returned, with **4.324 seconds spent
inside reads during 4.678 seconds observed**. Eighteen reads took at least
100 ms; the longest took 338.5 ms. Output writes occupied 0.259 s on another
thread and mostly overlapped those reads; output-only occupancy was about
0.028 s. An unfinished read at detach is excluded. This is evidence of
input-wait dominance during this short window, not a claim that 92.45% of
all playback delay was caused by disk I/O. Tracing may perturb scheduling;
the independent one-second page-wait samples agree with it.

The NAS source disk was about 100% busy with **63–66 MiB/s aggregate reads**,
24–28 ms average block-read latency, and no writes in the post-start windows.
Before playback it supplied about 102 MiB/s to a different read pattern.
This is not the ceiling of the 10 Gb/s link; it is the observed disk service
under concurrent requests. Disk utilization alone does not diagnose drive
failure, fragmentation, or a precise causal share for each reader.

Two competing streams were observed:

- The `lab4` daemon directly read the **same Craft source**, sequentially from
  byte zero in 256 KiB calls, while playback read near the resume offset.
  Its FD advanced from 591,921,152 at 19:14:00 to 2,383,937,536 at 19:15:30;
  matched daemon physical reads were about 16–18 MiB/s after startup.
  The matching fragment-index analysis attempt started at **19:13:54.928**
  on `lab4` and ended at 19:16:46.525 with `foreground_preempted`, without
  an artifact job/result generation. This brackets the observed direct
  source reader and supports pre-artifact attestation attribution. The
  durable current phase has overwritten its former phase; it is not a
  contemporaneous `hashing` sample.
- `lab6` concurrently performed roughly 16–19 MiB/s of NFS reads in the
  later windows. A held Game of Thrones FD alone does not identify the
  executing reader: this sampler excluded non-FFmpeg helper descendants.
  Those NFS reads are measured; the specific background task remains unproven.

The source admission has a concrete blind spot: `pretranscode_worker_idle`
checks live demand and encoder/software permits, while copy production holds
none of those encoder permits. `analysis_source_may_continue` can therefore
regard a copy session as idle; its other allowance for live artifact waiters
is a separate admission branch. The job history does not identify which
branch admitted this particular hash. Full HEVC attestation occurs before
artifact-job submission, so checking only artifact workers misses it.

The new traces have no background-exclusion counterfactual. The morning
scanner A/B/A control in the canonical RCA independently proves a contention
mechanism; it is not reused as an exact allocation for this different replay.

**What `0.644x` actually means.** `recent_rate_step` in
`transcode/producer/progress.rs` updates a recent EWMA (70% previous estimate,
30% new rate, progress intervals 400–5,000 ms). It has no age-out that turns a
stale last value into unknown. It includes input delay, intentional pacing,
and downstream stalls. It is neither a 64-second average nor a disk speed.
The 18:55 attempt also logged Raft deadline/unreachable events near the
client deadline; there was no simultaneous syscall/output-grant capture to
exclude a publication/backpressure contribution. A later healthy cluster
snapshot cannot retroactively exclude that transient contribution.

**Why quick segments still take seconds to become playable.** The controlled
replay had a complete first segment at 260 ms and opened its writer gate at
1.742 s, but its first published snapshot arrived around 12 s after producer
installation, after accumulating 50.842 s of media. The writer's 12 s media
gate and publisher's 48 s media runway are separate tests. They are not
fixed wall-clock sleeps and their values must not be added. Both must change
for the proposed few-second cold-start contract; changing only the writer
cannot remove the publication wait.

## 720p — a smaller output still reads the large source and waits for a family

The source remains the 68 GB HEVC remux. Selecting 720p reduces encoded
output, but it does not provide a smaller input file. The continuous-quality
path starts primary video, companion video, and shared audio together. The
video jobs encode H.264 QSV; the audio job demuxes the MKV and converts the
selected six-channel AC-3 to AAC. An audio-only FFmpeg job still has to read
and demux the interleaved source; its output bitrate is not its input demand.

In the clean capture, the audio process spent all eight sampled input-thread
observations in page wait, used only about 0.003 CPU cores on average, and
read about 3.75 MiB/s logically. Its write counter stayed at startup-diagnostic
size for the seven-second sample. Both video write counters were also flat;
they used about 1.6 CPU cores each on average and much more in the final
sample. Thus audio is an additional blocked dependency, **not proven to be
the sole last child**. This capture does not establish that video encoding
or software decoding is free of cost.

Do not multiply physical disk traffic by three: nearby source offsets share
cache pages. Over the seven-second sample the three jobs read 154.43 MiB
logically but incurred 56.38 MiB of charged physical reads, close to the
56.61 MiB received by the worker's NFS mount. The container had no CPU quota
or throttling; interval I/O pressure was high. Those facts rule out a
container CPU cap and identify real source waiting; they do not substitute
for an actual GPU/decode utilization measurement.

This capture had different background work from the Original replay.
`lab4` and `lab6` NFS read counters were flat during producer lifetime.
`lab3` issued five concurrent streams of tiny, widely spaced reads of a
Shameless source on the same disk: only about 0.165 MiB/s, with ~198 ms mean
NFS RPC latency. The disk was still about 100% busy, with 22.34 MiB/s and
~30 ms average block-read latency. Low byte volume does not make random
source I/O harmless. The matching `lab3` log identifies file 6512 subtitle extraction: 4,203 reads,
11,522,302 bytes, 38,589 ms, finishing at 19:31:44.618. Its durable analysis
attempt began at 19:31:05.765 on `lab3`. This corroborates the observed
Shameless descriptor and 64/4,096-byte parallel-read signature. Its precise
share of the playback delay has not been isolated.

The code exposes why that work can be disruptive: `mkv_sparse.rs` uses up to
eight scoped reader threads, reading 64-byte cluster headers and 4,096-byte
subtitle blocks at distant positions. One background source reservation
therefore admits multiple simultaneous seeks. `run_subtitle_work` requests
cancellation on playback pressure and correctly retains ownership while
awaiting work, but the sparse read loops do not receive that cancellation
token. Header reads have no cancellation/deadline check; block reads have
a 120-second deadline only. Thus a cancellation request cannot promptly stop
new sparse reads. This is an admission/preemption granularity defect, not
evidence of detached unowned threads, nor proof that cancellation was
actually requested in this captured attempt. Return a distinct canceled
result: treating cancellation as `None`/not-indexed invokes full-demux
fallback and can make source contention worse.

### The dependency barrier turns slow initialization into a client failure

```text
continuous-sessions succeeds
  -> starts primary video + companion video + audio
  -> quality-family waits for verified init for every child
  -> request budget ends: 503 segment_pending, Retry-After
  -> continuousQualityFetch discards typed error body
  -> openContinuousQualitySession catches error and releases session
  -> DELETE removes reader demand; producers stop
  -> later init watchdog finds no running producer
```

`vod/serve/delivery.rs::verified_continuous_family_before` verifies every
child initialization before returning the description. This preserves the
advertised family contract, but makes every child a startup dependency,
including the companion rendition. In `web/player/continuous-quality.js`,
`continuousQualityFetch` throws a generic error for non-OK responses without
retaining the typed `segment_pending` body. The caller's catch releases the
newly created session on any family fetch error. The existing open-session
retry path expects a typed stream failure, and its create-phase retry policy
does not cover this pending-family case. Parsing the body alone is therefore
insufficient; a bootstrap-specific bounded retry policy is needed too.

The HAR establishes the order: family GET at 19:31:06.042, 503 about
19:31:14.093, DELETE at 19:31:14.095, owner stopped/deleted by 19:31:14.133.
The server's 30-second init watchdog fires at 19:31:36.088, **after deletion**.
It is a consequence of abandoned work, not evidence that FFmpeg crashed
before the family request failed. There is no ordinary fallback on this 503;
404/405 compatibility fallback is a different path.

The first 14.560 s capture has an 8.16 s gap between create completion and
first manifest dispatch, plus later attachment/presentation work. The source
barrier is consistent with that gap; without its own network capture it is
not an exact measurement of family-GET duration. Its audio diagnostic shows
slow initial progress: no output at 4.5 s and 5.50 s of media after 10.01 s.
However, the last five seconds of that diagnostic produce about 1.044x; the
cumulative 0.55x includes startup and cannot establish sustained 0.55x supply.
The first frame has only 2.6 s runway, then real zero-buffer stalls occur at
film positions 436.4, 440.4 and 444.4 s. That proves buffer starvation. The
attempt lacks synchronized
per-child inventory/read/CPU evidence to allocate the later stalls exactly
between input, decode/encode, and scheduling. The clean failure supplies an
additional proven bug; it does not erase this continuity evidence gap.

### Cropped 4K SDR takes the software-decode default

The item response in the HAR records **3840×2076, HEVC, SDR** for file 6964;
the producer diagnostic independently reports Main10/yuv420p10le at those
dimensions. `transcode/decode.rs::preferred_backend` classifies HEVC as heavy
only when HDR or source height is at least 2160. `transcode::heavy_source`
uses the same height-only threshold for pipeline selection. Width and bit
depth do not enter that test. Cropped 4K therefore misses the acceleration
policy and defaults to software decode with QSV encode on the ordinary SDR
path. No `PLURX_HWDECODE` override was configured in the running container.

This is a specific planning defect to fix, independent of source contention.
That default would add software-decode and CPU-filter work to both video
children while they compete for input. The
complete frozen producer argv was not retained for the first attempts; their
decoder choice is inferred from the default planning path. The later repeat
below directly captures the executed flags. Neither establishes a numerical
share of the original stalls. It
cannot explain Original-copy starvation, which does not decode video.

### The unlocked-Mac repeat confirms software decode and repeats the delay

After the user unlocked the Mac, a fresh AC-3/720p Resume at
**20:04:13.191 UTC** ran on `media1`. The same resume region was retried; this
is called a warm repeat for identification, not proof that every required
page was cached or that the disk was idle.

The client reports **14,765 ms TTFF**, with **2.5 s initial runway**. The HAR
shows decision 155 ms, candidates 67 ms, session POST 981 ms, then a
**7,410 ms family GET** beginning 20:04:14.504 and returning 200. This repeats
the long initialization barrier even when it finishes just inside the
server's pending-response budget, unlike the 19:31 failure.

Both video children actually execute `-hwaccel none` with `h264_qsv`.
The primary graph scales on CPU and then uploads frames; the live playback
information independently reports **CPU video processing**. Thus software
decode plus CPU filtering is directly observed in this generation, not
inferred from the encoder name. Primary video averages about **3.75 CPU
cores** across the 88-second sample; the short-lived companion averages
3.25 cores over 11 seconds. The primary also has an input page-wait thread
in 86/89 sampled snapshots. CPU work and source waits coexist; those counts
are sampled presence, not exclusive time percentages.

The audio child has substantial media writes by 20:04:20.444. Both video
write counters remain at diagnostic size through 20:04:21.442 and show
substantial writes by 20:04:22.437; family success completes at about
20:04:21.914 within that interval. **Media-sized video writes appear later than
audio writes in this repeat.** Sampling cannot distinguish which video child is
last; it does not measure exact init completion from write bytes alone.
A later audio reposition/restart is also captured and remains unattributed.

The disk remains about 100% busy at 55–62 MiB/s, with competing `lab4`
NFS traffic about 22–31 MiB/s during playback; the executing remote task was
not identified. This is not an isolated CPU benchmark. No VOD inventory
series was captured, so write-byte rate is not called media production rate.
After about 100 seconds of presentation, native playback information
reports **zero buffering interruptions**, 2,538 frames, 58.4 s device buffer
and 98.5 s ready on the server. This is a successful short continuity sample,
not proof about the earlier stalling attempt or long-run acceptance. The
diagnostic player was paused and closed afterward; AC-3 remains selected
on the title page.

## Resume — an unsafe quality handoff meets unqualified presentation observation

The user reported moving video beneath the buffering overlay for about 15 s,
followed by an automatic pause/error and “Playback is still stalled.” This
adds three application defects to the earlier startup work. It is not an
empty-buffer diagnosis, and copying HEVC does not invoke the cropped-UHD
software-decode policy.

| 20:32 incident, UTC | Captured event |
|---|---|
| 05.104 | Established 720p loader resumes at film 1003.9 s with **60.9 s buffered** |
| 05.280 | Periodic Auto reports retaining 720p for its upgrade observation window |
| 05.406–05.825 | A separate attempt selects Original/native copy; predecessor VOD is superseded |
| 07.101–07.348 | Copy publishes **53.845 s** with `consumed_end_ms=0`; native HLS attaches |
| 07.357 | `media_waiting` raises the buffering surface |
| 15.361 | Presentation-stall report after 8 s, with **47.4 s buffered** |
| 27.358 | Recovery restarts the session after the 20 s observation window; **41.4 s buffered** |
| 36.955–48.883 | Successor reports another presentation stall with **40.1 s buffered**, then exhausts recovery |

The visible moving picture is the user's observation. The historical log
alone cannot identify which attachment supplied every displayed frame. The
instrumented reproductions below establish the application decisions and
separate their causes; they are not a reconstruction of every browser event
in that first incident.

### Resume bypasses the normal retained-incumbent Auto handoff

After an explicit pause of at least 60 s, `togglePlay` resumes the established
loader and independently calls `resumeQualityBoundary`. Its natural-boundary
picker can admit an unmeasured Original candidate for a trial. The boundary
then calls `seekTo` with that candidate. That branch uses generic
`requestPlaybackMediaChange`; the copy path invokes the predecessor-retirement
callback before attaching the new source. It does not require replacement
frame proof or the Original transfer trial's link-margin proof.

Normal periodic Auto changes take `switchAutoCandidate` →
`requestQualityChange` and the prepared handoff owner, which retain the
incumbent until replacement proofs pass. The Resume path bypasses that owner.
The periodic “720p retained” beacon therefore does not veto the other picker.
The defect is **destructive execution of an optional quality upgrade before
its replacement is proven**, not the mere existence of pause-boundary quality
reconsideration.

A private Node fixture executes the shipped picker, boundary, seek and generic
change functions with synthetic candidates and stubbed media/network adapters.
It proves the routing bypass. Its 60.9 s buffered value is descriptive fixture
input, not modeled browser presentation, and its retirement stub is validated
against the real source call path. It calls the boundary directly rather than
exercising the 60 s trigger. A separate live test covers that trigger: verified
720p playback was paused at **20:48:08**, Auto was selected for the test, and
Resume at **20:49:29** resumed 60.0 s of buffered 720p before superseding it with
native Original. That attempt subsequently hit supply recovery; it is not
used as the proof of the earlier loaded-buffer presentation failure.

**Proposed fix:** invoke the existing voluntary retained-incumbent owner from
`resumeQualityBoundary` **before minting a synthetic seek intent**. Decline the
optional upgrade when preparation/proof is unavailable, while the resumed
720p stream continues. Actual viewer seeks and reopening a retired paused
session still fulfill their requested position. Carry generation, attachment,
intent and cancellation fences through selection, preparation and commit;
a stale Resume decision cannot replace or retire a newer operation. Preserve
the existing successor frame and transfer-trial proofs.

### API presence is treated as operational presentation evidence

The instrumented native retry `a9` exposes the mismatch directly. The observer
records only whitelisted browser state; it does not alter media bytes or
callback results. To retain the failed source long enough to inspect it,
a temporary tab-only replacement held the legacy `persistentWait` recovery
function. It automatically restored after 65 s. The independent native HLS
startup deadline remained active and ultimately stopped playback.

| Same `a9` attachment | Early moving sample | Later sample |
|---|---|---|
| Element clock | 1.328 s | **35.808 s** |
| Browser decoded-frame count | 25 | **869** |
| App callback-receipt counter | **975** | **975** |
| Callback API flag / observer armed | true / true | true / true |
| Metadata anomalies | 0 | 0 |
| Pending seek target-frame proof | missing | missing |

The pending seek's callback floor is also 975. Its clock allowance reaches
8,000 ms, but no target-frame receipt arrives. At **20:52:31.735**, the native
startup owner raises `startup_exhausted` and pauses the element: “The native
playlists were ready, but no presentation progress was observed.” That
application decision occurs after the measured clock/decoded progress.

`armHitchDetector` sets `controlHasFrameCallbacks` from
`!!video.requestVideoFrameCallback`, which establishes API availability, not
operational delivery for this source. `playbackProgressTick` then exclusively
uses its receipt counter and requires both clock and counter advancement.
Seek settlement likewise requires a timestamped callback while that flag is
true. A silent observer therefore prevents seek settlement, surface clearing
and native startup completion; recovery/deadline owners eventually stop the
media. This is the application-level cause of the false certainty in the
buffering/error presentation: **absence of observations is treated as proof
of absent playback**.

Clock and decoded frames do not independently prove compositor presentation
or an exact seek landing. They do demonstrate conflicting progress evidence;
this report does not relabel them as trusted target-frame receipts. The user
reported visible movement in the original incident. The instrumented retry
changed the legacy recovery lifecycle, so its 40 s startup stop must not be
presented as an unmodified reproduction of the original 8/20 s sequence.

The later [native callback investigation](NATIVE-CALLBACK-DELIVERY-RCA.md)
resolves this subscription-lifecycle cause. A raw request registered while
native seeking remained pending across `seeked`; the `id != null` guard
prevented renewal. Cancel-to-empty/resubscribe restored the first callback in
32 ms, and an independent rearm-only control restored 36 receipts in 1.504 s
without media replacement. The new RCA records both experiments and the
reviewed repair contract. The earlier `a10` late-rearm observation was not
proof by itself; those later controls supply the causal evidence.

**Repair draft:** apply the [owned subscription renewal contract](NATIVE-CALLBACK-DELIVERY-RCA.md#revised-fix--one-multiplexer-per-element-renew-at-native-discontinuities).
The original native deadline concludes `observation_unavailable` when fresh,
owned decode/clock progress conflicts with silent callbacks. It withdraws only
that owner/generation/intent's observational progress claims and shows a
nonblocking Try again / Close notice. It does not pause, retire or stop loading,
certify presentation, or settle a seek. Actual freezes and authoritative errors
retain normal bounded recovery. Web, Swift and Kotlin share the withdrawal
contract; regressions are drafted, not run. Final review and native runtime
acceptance remain pending; see the [implementation ledger](PLAYBACK-STARTUP-IMPLEMENTATION-STATUS.md).

A separate source defect is reproduced in the same area: attachment replacement
without a pending seek (one reachable caller is subtitle burn-in) can leave
`queuePlaybackFrame` bound to the predecessor. Its first replacement callback
clears the subscription, fails the attachment check, and never requeues; the
API flag remains true. Rebind the observer on every committed attachment and
retire only the matching subscription. This is a genuine lifecycle defect,
**not attributed to the Resume incident**, whose captured attempts had seeks.

### Native replacement reports the predecessor clock in the new timeline

A later unmodified native seek/recovery reproduction (`a11`) establishes an
additional timeline-ownership defect. `attachSession` changes the session and
origin, but preserves the title-lifetime `started=true`. The native attachment
wait pauses the predecessor yet keeps its source, clock and buffers until the
new native playlists are ready. `playbackControlSnapshot` uses its provisional
position hint only when `!started`; it therefore adds the new origin to the
old element clock and credits old buffer ranges to the replacement.

The captured browser state and shipped snapshot function reproduce:

```text
new origin                  1233.816 s
predecessor element clock   +183.395 s
reported film position      1417.211 s
actual requested seek       1234.749 s
predecessor buffer           39.8 s -> classified as presentation wait
```

The server's rolling position projection reads `position_ms` even for Seeking
and subtracts the new origin. With that reproduced snapshot, it sees **183.395 s
consumed**; the Conservative initial window then requires **231.395 s of media**
(183.395 + 48), rather than the first window near the actual target. The
snapshot fixture executes the shipped JavaScript; the server arithmetic is
reproduced from source formulas, not Rust execution. A target-anchored comparison preserves the 933 ms origin-to-target offset:
at 1x it requires about **48.933 s**, not exactly 48 s. The fixture buffer
span is synthetic, seeded with the logged 39.8 s runway; its old clock, new
origin, started flag and pending target come from captured browser state.
The exact historical accepted control-request body was not captured, so these numbers establish
the reachable feedback defect, not a verified persisted demand value.

The `a11` worker produced its first segment in **90 ms** and opened the writer
gate in **284 ms**, but no first snapshot or native-source readiness is logged
before the 20 s recovery restart. The browser also labels the predecessor's
buffer as a presentation wait before the successor source is attached. Thus a
replacement can manufacture both excessive publication demand and a premature
presentation diagnosis from media state it does not yet own.

**Exclude the original `a3` from this attribution:** its first snapshot explicitly
has consumed zero and 53.845 s ready, followed by native attachment. The stale-
clock mechanism is additional, not a replacement explanation for that attempt.

**Proposed fix:** use attachment-owned pending-source position and buffer state
until the native source and requested position actually execute. Do not credit
predecessor buffered ranges or run successor presentation recovery before
that boundary. Resetting `started=false` alone is insufficient: an old-source
media event can reassert it. Fence observations by source/attachment execution.
Also qualify a server-side Seeking projection based on the explicit seek target
rather than the stale element position; preserve existing accepted demand
contracts across clients, and test this as a separate defensive change.

Required regressions cover long-pause optional upgrades with unavailable proof;
newer seeks/pauses/quality changes racing a staged upgrade; native clock and
decoded progress with silent callbacks; a real freeze without independent
progress; stale observer callbacks and missing target-frame proof; native
replacement with a large nonzero predecessor clock/buffer; and successful
recovery without either early teardown or unbounded waiting. Verify the
**first actual control request emitted while native readiness is delayed**,
including predecessor events arriving after replacement; a snapshot-helper
test alone cannot prove what reaches the server. Measure the
whole Resume-to-presented outcome and continued playback, not session creation.

All temporary browser instrumentation was removed by reloading the title page.
The diagnostic playback was closed and the pre-test Auto preference restored.
No production startup fix or server setting was changed by these tests.

## Kernel reports — isolate the capability probe and the unrelated denials

The kernel iHD fault for thread 3897697 correlates, using journal realtime,
with **18:38:02.239 UTC** and the application's 18:38:02.248
`HEVC Main10 VA-API decode probe` exit by SIGSEGV. The daemon then records
`hevc_main10_vaapi_decode=false`. It predates the 18:53/18:55 playback attempts;
no matching GPU crash was observed during those retries or the clean 720p
capture. A failed capability probe can affect later decoder selection, so
this report does not assert it has no indirect performance effect. QSV
encoding in an argv does not establish hardware decoding. Source tracing
also shows this dedicated probe flag is consumed by the processed-Dolby-
Vision path, not as the ordinary SDR QSV decoder selector. Ordinary HEVC planning prefers QSV only under the flawed heavy-source
predicate described above; only the actual resolved backend/complete argv
can establish which path this generation used. The first sampler intentionally
omitted hardware flags, so absent flags there prove nothing. The expanded
repeat sampler records them and proves software decoding for that repeat.

All **754 AppArmor denials in the retained kernel buffer** were for profile
`who` opening `/etc/nsswitch.conf` or `/etc/passwd`. None named Cinema,
FFmpeg, a media file, or a GPU device. They supply no evidence of the playback
blockage; disabling AppArmor would not address the measured mechanisms.

## Proposed repair — retain the startup target across source, family and client

The existing M1 and mandatory M3 remain necessary. Add the continuous-family
lifetime repair to their integration and qualification scope. No startup
runtime code/configuration was changed during this investigation.

| Repair | Required behavior | Evidence that must pass |
|---|---|---|
| M1, all source consumers | Reuse the replicated source ledger. Foreground rolling/copy and continuous children register the actual shared source domain. Background attestation/scanning yields cooperatively at bounded read boundaries, including remote nodes and helper/thread reads; start foreground through accounted overlap, without waiting for a whole hash to finish. Propagate cancellation into sparse header/block reads with a typed canceled result, retain joins/claims until outstanding reads settle, and budget parallel random reads by storage domain. Persist restart-safe hash progress and retain the prior fair-sharing contract. | Same-disk local/remote hash, sequential scan and tiny random reads; initial and sustained playback plus continuing background progress when supply permits. Observe actual source reads stop/resume, not just a canceled API state. |
| M3, writer and publisher | Admit the first playable, decodable window without waiting for 12/48 s of media. Keep the steady continuity reserve separate and retain generation, ownership, atomic publication and capacity correctness. | Cold/warm copy with first complete segment, gate, first published playlist, first frame, and early stall timings from one clocked attempt. |
| Continuous startup | Prepare the selected video and required audio first. Defer companion work and its advertisement until verified bytes exist, with a versioned/generation-safe family update. This proposal requires deferred companion advertisement; the current all-child barrier cannot also promise independence from a delayed companion. Do not advertise an unverified companion. Evaluate shared input/demux where it reduces repeated input work, using physical read counters. | Selected startup independent of an artificially delayed companion, followed by verified optional quality availability, seeks, quality switches, failures and canceled generations. This is a contract change, not simply skipping the all-child check. |
| Pending family lifetime | Preserve bounded typed error bodies; retry eligible pending-family responses against the same session under the original startup deadline. Preserve ownership/control lease and cancellation during retries. Release only on actual cancellation, fatal failure or final exhaustion; no resetting the clock or creating duplicate producers. | Fake 503 `segment_pending` then success must attach once without DELETE; repeated pending exhausts the original budget and cleans up once; stale responses/owner changes cannot attach or delete a successor. |
| Cropped-4K decoder policy | Use one source-complexity rule that accounts for cropped UHD dimensions and relevant codec/bit-depth facts; preserve renderer restrictions and measured hardware capability requirements. Keep decode and filter planning consistent. Do not force hardware decoding for all sources based on encoder name. | Regress 3840×2076 Main10 SDR alongside full-height UHD, 1080p/light sources, HDR, missing/rejected capabilities, forced software, and renderer restrictions; retain actual decoder/CPU/input-wait evidence on the movie. |
| Capability and observability | Record actual decoder/filter/encoder choice and probe failure, per-child first-output/init times, input-wait and deliberate-hold time, and recent-rate observation age. Preserve source supply versus publication-grant timing. | The 0.644x condition must be diagnosable from one attempt; a stale EWMA must not masquerade as a fresh independent capacity measurement. |

Acceptance remains **2 s ready / 5 s supported cold** from the original
gesture to first presented media, plus continued playback, as defined in the
canonical proposal. A quick error, a later prepared retry, a server segment without a
client frame, or a smaller output resolution does not satisfy it. Retest
Original+AC-3 and 720p from cold and warm conditions with a fixed worker,
record cache/workload rather than assuming it, and retain at least the early
continuity interval. Include input starvation, a stalled companion, an audio
startup delay, transient Store/publication latency and client cancellation.

## Evidence custody and review

Primary captures live in the private task artifact folder; raw HAR contains
authentication and is excluded from review packets and the repository.
Useful credential-free derivatives are `copy2-derived.json`,
`copy2-strace-derived.json`, `copy2-analysis-job.json`, `720clean-derived.json`,
`720clean-network-timings.json`, `720clean-subtitle-source.log`,
`720clean-subtitle-job.json`, `720clean-source-facts.json`,
`warm720-derived.json`, `warm720-network-timings.json`, and
`warm720-ui-observation.json`.
Raw process samples establish source FDs and CPU/wait counters; NAS counters establish disk service; HAR timings
establish the family-response/deletion order. These are manually assembled
incident evidence, not automated qualification receipts.

Astra's first evidence review required six corrections: do not transfer the
controlled replay's attribution to the historical 0.644x attempt; do not call
read occupancy an overall causal percentage; do not identify audio as the
sole blocking child; distinguish the first attempt's inferred family delay
from the later HAR; do not assign all disk contention to the tiny random
reader; and distinguish media thresholds from wall time. Those corrections
are incorporated above. Astra then reviewed this final draft against targeted
evidence and source,
found no new P1 and one P2 inconsistency: its optional retained-family
alternative could not satisfy delayed-companion independence. **Addressed:**
this proposal now requires deferred companion advertisement with a versioned
contract. Astra also requested conditional wording for the code-derived
software-decode effect; that wording is incorporated. The executed-decoder
caveat remains. Astra's disposition was ready for Fable after those changes.
This is a documentation/source review, not implementation approval or runtime
qualification.

Astra also reviewed the added unlocked-Mac repeat. **P2 addressed:** media-
sized writes do not identify verified initialization order, so the text now
reports write observations without naming a last verified child. The short
continuity interval is stated as about 100 seconds to avoid conflicting
frame-count and observation-time estimates. With those corrections, Astra
again found the report ready for Fable; the historical attribution limits
remain.

Astra reviewed the Resume addendum, its reproductions and cited source: no
new P1/P2 blocker. Its two precision notes are incorporated: retain the
933 ms target/origin offset in publication acceptance, and verify the first
actual outgoing control request during delayed native readiness, including
stale predecessor events. This review establishes no historical wire-demand
acceptance, browser callback-delivery cause or runtime-fix qualification.

The subsequent native callback RCA supersedes the callback-lifecycle open
point in the prior review. Fable's 2026-11-02 review revised its proposed
repair: all repository callers use one per-element callback multiplexer, with
native-only, event-driven discontinuity renewal at qualified readiness and no
progress-evidence precondition. The earlier cohort-reset contract and its
acceptance cells are superseded. Implementation and runtime qualification
remain pending; see [the Fable disposition](NATIVE-CALLBACK-FABLE-REVIEW.md).

Continuous-family acceptance also requires a client binding that accepts a
family without its companion and client consumption of the versioned family
update; server-only deferral fails at `Continuous family catalog binding`.

For cropped UHD, share the duplicated height-only HEVC rule in
`decode.rs::preferred_backend` and `transcode/mod.rs::heavy_source`; decode and
filter planning must agree for 3840×2076 Main10 SDR. Both changes are required,
not built claims.
