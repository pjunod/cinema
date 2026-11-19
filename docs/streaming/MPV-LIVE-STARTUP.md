# MPV live startup admission

**Owner:** Codex; Sol implements the Linux Player request and request regressions.
**Status:** current main is integrated. The installed ROG baseline passed an actual OSC seek. Patched Play and seek measurements are pending.
**Updated:** 2026-11-19.

A normal Linux Player seek recreated a live copy session. The first frame
arrived 26.993 seconds after the OSC click. A complete 2.795-second fragment existed;
22.281 seconds elapsed from the writer playlist gate opening to the first
served playlist. The create request omitted its consumer transport, so MPV
inherited Safari's 48-second native presentation admission requirement.

## Measured cause

Retained production phase logs distinguish writer readiness, publication and
init validation. These are server observations, not optical picture measurements.

| Start | Writer gate to first snapshot | First snapshot media | Init validation |
|---|---:|---:|---:|
| Initial play | 10.803 s | 49.675 s | 122 ms |
| OSC seek | 22.281 s | 49.759 s | 13 ms |
| Return seek | 5.165 s | 49.675 s | 108 ms |

All three used `NativePresentationWindow` with `bootstrap_ms=48000`.
The copy writer's own gate was already open. The seek's first snapshot and
response admission immediately precede MPV receiving the playlist MIME type;
client decode to first frame then took approximately 0.28 seconds.
Private logs and source identities are retained in effort E-3B754319C5.
No raw media identity, bearer session capability or host capture belongs here.

## Correction

Linux Player 0.5.2 sends the existing create-time `transport: "mpv"` on initial
start and session recreation. The server freezes the consumer policy at create.
MPV retirement remains conservative. Native/Safari, omitted and unknown
transports retain their existing native admission; hls.js keeps its own window.
Older servers accept `mpv` and retain conservative behavior.

First-cut publication is insufficient: FFmpeg reloads after the last segment’s
duration, then waits half the target after an unchanged reload. A 2.795-second
cut followed by an 8.1-second cut produced at 2x misses the first reload and
cannot be discovered until about 10.795 seconds. The initial candidate is held
and superseded by a producer/reload budget. This follows the selected consumer
version’s [upstream HLS schedule](https://github.com/FFmpeg/FFmpeg/blob/n8.1.2/libavformat/hls.c);
the [MPV tag](https://github.com/mpv-player/mpv/blob/v0.42.0/demux/demux_lavf.c)
does not establish every distribution/native patch in the selected binary.

The supported operating envelope assumes sustainable production at least as
fast as playback, complete cuts no longer than 16 seconds, 2-second minimum
refill spacing and a publication poll/commit opportunity within 250 ms after
eligibility. The scheduling/delivery allowance is 1 second, informed by retained
snapshot-to-first-frame observations below 0.9 seconds. These are conditional
service bounds: nominal polling and recent average speed do not guarantee future
scheduler, network, disk or producer behavior. Selected-build continuity remains
required for acceptance.

The first reload-window formula covered an unchanged reload's half-target wait,
but missed a steady-state case. At 4x, its 68-second floor admitted a 70.265-second
complete prefix. The deterministic replay's changed reload at 38.361 seconds
received the endpoint at 217.614 media seconds. That snapshot's last `EXTINF`
was 16 seconds, so the next reload was at 54.361 seconds. With a 233 ms seek
lead, known media ran out at `(217614 - 233) / 4 = 54345.25` ms, before that
reload. A changed playlist can already contain a stale endpoint.

For playback rate `r`, presentation target `T`, cut bound `Dmax`, minimum refill
spacing `P`, maximum eligible poll/commit gap `q` and scheduling/delivery allowance
`m`, let `c` be the completion time of the latest endpoint in a committed
snapshot and `p` its commit time:

1. A cut held by refill spacing can wait one `P` plus a poll, so `p-c <= P+q`.
2. The next cut completes by `c+Dmax/r`. Its commit must also wait until `p+P`,
   then at most another `q`. Delivery adds `m`.
3. The latest delivered endpoint can therefore lag production by wall time
   `A = max(Dmax/r, 2P+q) + q + m`.
4. A changed reload can read that stale endpoint and then wait up to `T` for
   another reload. An unchanged reload waits `T/2`, which is already covered.

```text
endpoint age envelope A = max(Dmax/r, 2P + q) + q + m
required media runway = ceil(r * (T + A))
T = Dmax = 16000 ms; P = 2000 ms; q = 250 ms; m = 1000 ms
```

The separate poll allowance inside `2P+q` covers the first commit's endpoint
age. The outer poll allowance covers the following commit. No alignment between
publication commits and polling phases is assumed.

| Playback rate | Required MPV media runway | Native media runway |
|---|---:|---:|
| 0.25x | 20.313 s | 48 s |
| 1x | 33.250 s | 48 s |
| 4x | 86 s | 124 s |

The computed MPV floor is not silently truncated to the reserve cap. Short MPV
admission additionally requires finite recent production at least equal to
playback; unknown or slow capacity, or a global scratch hold, retains the entire
native admission and complete-cut rounding policy. The actual first snapshot
must include the seek lead and ends on a complete cut. The 1x seek replay
therefore waits for five variable-GOP cuts ending at 34.582 seconds, rather than
publishing the earlier 26.482-second prefix.

Until a first snapshot is admitted, capacity is reassessed at the fixed original
production bank even after an earlier sufficient sample. A short MPV first
snapshot carries an explicit unpaid initial-capacity assessment to that bank.
A finite sufficient observation pays this debt; unknown speed does not. Native
admission at its original bank carries no short-start debt and preserves the
existing post-publication deadline behavior, including global scratch holds.

The existing flow controller owns steady production and reserve. Init and
complete-object validation, ownership/attempt fences and retired-object promises
remain in their existing paths. No timer, retry, watchdog or forced transport
capability masks missing media.

## Qualification follow-up

All 12 added MPV/bootstrap/capacity tests passed on source `a9dc89cf0`.
Failed-only CI then retained two blockers in unchanged paths: raw-probe
retirement and sharing-actor control. Base comparison passes both; the captured
failures alone do not establish their causal relationship to the startup change.

Source inspection found a separate accounting defect: `finish_joined` could
return a source-service error after joining its worker but before marking raw
retirement. The correction records proven physical completion while returning
the original semantic error. An incomplete reap/join still vetoes completion.
A deterministic real-process regression injects a source error after child reap
and verifies worker join, error propagation and zero remaining raw ownership.
The captured timeout's exact worker outcome remains unknown.

Four focused cases passed on the pre-integration source. These exercised
the real H1/H2 control path, the original retirement case, the joined-worker
error regression and the child-group reaping case. The sharing failure did
not recur. Its earlier cause remains unknown. The focused diagnostic changes
were removed before integrated validation. Runtime qualification remains
pending.

## Acceptance

The causal regression constrains complete inventory by production timestamps
and independently models FFmpeg’s reload schedule, including unchanged reloads,
short-to-long GOPs, seek offsets, supported playback rates and irregular bounded
publication poll phases. Both the original first-cut policy and the superseded
68-second 4x floor must fail their causal counterexamples. Actual publication tests cover
explicit/legacy demand, conservative unknown-capacity rounding, replenishment
and the initial capacity assessment after early publication. Safari’s fixture
remains a separate regression. Player tests cover initial remux/transcode,
normal seek, OSC film seek and quality recreation.

The Player run accounts for 120 tests. Successful cases were retained; only
55 failed methods were rerun in a Linux container after resolving Mac runner
socket-path/Bash limitations and package-version metadata. All 55 passed with
no skips. The 0.5.2 frontend package is built. The source-35def Rust run exposed the
steady-state reload counterexample and two publication/capacity test failures;
the revised bound, explicit short-admission debt and coherent worker-clock
fixture subsequently passed review and the added MPV cases. Integrated checks
and normal-route runtime acceptance remain open.

Before-change metrics were retained privately from the same lab node. Its
`plurx_ttff_ms` and `plurx_seek_to_picture_ms` series have zero observations, so
no historical latency distribution is claimed. Compare after-change server
phase timestamps with normal-player opening/MIME/first-frame timing; expect
`MpvReloadWindow` and a 33250 ms bootstrap at 1x with established capacity.
Unknown capacity should report the conservative 48000 ms requirement.

Device acceptance must establish advancing playback without the reload
counterexample, accepted controls through replacement and native mapping.
Synthetic tests or a PQ surface do not establish full Dolby Vision, every-frame
mapping or optical accuracy.

## Current-main integration

The installed ROG baseline ran main `6be46d14838a` with Player `0.5.1-1`.
An actual OSC seek accepted the replacement's first control at 4.67 seconds
and confirmed advancing native Dolby Vision at 11.35 seconds. The following
60.44 seconds captured 12 accepted control exchanges, zero HTTP 409s and zero
sampled cache pauses. The capture began after Play. These results establish
the seek baseline for the patched runtime.

Main `60443f341` includes the joined-reader retirement correction in
`b5c3e8f24`, along with a regression covering worker completion and error
propagation. The integration keeps main's code and regression.
