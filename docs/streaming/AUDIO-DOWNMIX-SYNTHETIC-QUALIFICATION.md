# Downmix qualification — measured synthetic gains, not listening acceptance

**Status:** synthetic subset measured; real-content and device acceptance open
· **Measured:** 2026-09-30 · **Source:** effort `8a7dbf5337584b2bb0556d0b617fef48122def2e`
· **Model / session:** gpt-6.1-sol / agent:/root/s02_batching_sol61

Companion to [Audio resolved independently](AUDIO-RESOLVED-INDEPENDENTLY.md)
§3.4 and §5.4. This supplies real numbers for its two synthetic requirements
and three named layouts. The measurement itself is attributed to the exact
source above, not a later implementation tree. Accompanying M2 work propagates
the existing typed delivery decision; it does not enable these candidate
pan/limiter recipes, change source-layout facts, mint client claims, or change
the AAC VOD sample lattice.

## What the shipped engine actually did

The proposed `0.707` centre coefficient produced a −3.0116 dB split **on
each output side**, not merely an integrated stereo loudness result. The
FC-only source measured −20.0000 dBFS RMS / −20.4 LUFS; each pan output
measured −23.0116 dBFS RMS / −23.4 LUFS, within the plan's ±1 LU of
−23 LUFS. No gain adjustment is supported by these numbers.

The shipped engine's actual default did **not** reproduce the plan's
universal 7–8 dB dialogue attenuation claim. Default float output measured
−23.0103 dBFS RMS / −23.4 LUFS per side. Default AAC 160 kb/s decoded
output measured approximately −23.2445 dBFS RMS / −23.9 LUFS; explicit
pan measured approximately −23.2472 / −23.9. The near-identical FC results
are evidence against promising dialogue improvement on this engine, not a
claim that all source mixes sound identical.

Unbounded pan exceeded full scale on the coherent all-channels stimulus.
An explicit limiter is therefore needed for this synthetic worst case.
Limiting the pre-encode samples to −1 dBFS is insufficient to keep the
**decoded AAC** samples at or below −1 dBFS; AAC reconstruction overshoots.
A −2 dBFS pre-encode limit met that delivered bound on these fixtures.
It is a measured candidate, not a universal codec/content guarantee.

## Exact engine and isolation

The host was `nynuc`, Linux x86_64. Read-only identification of its running
image preceded a new, isolated container from that exact local image:

| Object | Identity |
|---|---|
| Image ID | `sha256:5b6df3d7949fc34a21dc81bf26945a0b6dbb89c6ca48ff64d2f348adc48daac2` |
| FFmpeg | `/usr/lib/jellyfin-ffmpeg/ffmpeg`, `8.1.3-Jellyfin`, GCC 12 / Debian 12.2.0 |
| FFmpeg SHA256 | `90004301255382e1beb441a294fa8b75ac7fbb54678837f224d3458032e38787` |
| ffprobe SHA256 | `2f270d6f6f97bfb4167ee8ab00d4e17ecd18bd22069f4d270f7af41c06e11ac2` |

The owned root was `/var/tmp/plurx-s09-m4.mKtfS7`, on local ext4. Container
`plurx-s09-m4-mKtfS7` had no network, all capabilities dropped,
`no-new-privileges`, the ordinary SSH user's numeric UID/GID, CPU 1,
CPU shares 128, memory 1,073,741,824 bytes, memory+swap the same value
(own cgroup swap prohibited), and PID limit 128. Only that root was mounted;
no production data, configuration, auth, repository credential, `.git`, or
device was mounted. FFmpeg used one thread and one filter thread. Python
orchestrated on the host; actual media work ran inside the capped container.
Collector phases had 900 / 300 / 120 / 120-second outer deadlines and individual
commands had 120-second deadlines; container lifetime was bounded to 1,200 s.

Before execution, MemAvailable was about 40 GiB and free disk 36 GiB.
The host's existing approximately 5.8 GiB swap occupancy was recorded, not
represented as zero host swap. No swap was authorized for the owned container.
After execution, MemAvailable was about 54 GiB and free disk 47 GiB; these
are observations, not a claim that this task reclaimed space. All media
commands succeeded. Exact container teardown verified `running=false` and
`OOMKilled=false`. Its idle `sleep` PID returned 137 after bounded Docker
stop; that is teardown, not an FFmpeg failure or OOM. Media and diagnostics
remain in the owned evidence root; no production process was stopped.

## Inputs and channel facts

All inputs are 12 s, 48 kHz, floating-point PCM WAV. ffprobe actually reports
six channels with `5.1`, six with `5.1(side)`, and eight with `7.1`;
the script does not infer speaker positions from the count. The retained
[PCM receipt](../../tests/fixtures/audio-downmix/pcm-results.json) includes
every source stream document and per-source-channel measurements.

FC-only pink noise used `anoisesrc=color=pink:seed=20260930`, then one
measured gain `0.5153070544418407` to calibrate RMS to −20 dBFS. Only FC
(actual channel 2 in these verified layouts) is populated; all other
channels are zero. The pink noise's integrated loudness is −20.4 LUFS,
not an invented equality between RMS dBFS and LUFS. Its source sample peak
is −6.48627 dBFS.

The worst case places the **same phase-coherent 1 kHz sine** on every
channel, including LFE: amplitude `10^(-3/20)` = −3 dBFS sample peak,
−6.0103 dBFS RMS per channel. This intentionally makes retained-channel
sums add constructively. Six or eight channels are generated according to
the explicit source layout; there is no ambiguous sixteen-channel input.
The named matrices omit LFE.

## Matrices and measured before / after

These exact strings use `=` (no pan renormalization):

```text
5.1:       pan=stereo|FL=FL+0.707*FC+0.707*BL|FR=FR+0.707*FC+0.707*BR
5.1(side): pan=stereo|FL=FL+0.707*FC+0.707*SL|FR=FR+0.707*FC+0.707*SR
7.1:       pan=stereo|FL=FL+0.707*FC+0.5*SL+0.5*BL|FR=FR+0.707*FC+0.5*SR+0.5*BR
```

All values below are per output side. Floating-point output preserves values
above full scale, so the count is precisely **samples at or above full
scale**, not a claim that a float file has already been hard-clipped.
Actual integer playback would require clipping or another protection stage.

| Layout | Unbounded pan peak dBFS / over-scale count | Float limiter −1 dB peak / count | Decoded AAC limiter −1 dB peak L / R | Decoded AAC limiter −2 dB peak L / R |
|---|---|---|---|---|
| 5.1 | +4.65474 / 360,000 each | −1.00000 / 0 each | −0.49611 / −0.51321 | −1.28321 / −1.28829 |
| 5.1(side) | +4.65474 / 360,000 each | −1.00000 / 0 each | −0.49611 / −0.51321 | −1.28321 / −1.28829 |
| 7.1 | +5.64977 / 360,000 each | −1.00000 / 0 each | −0.47151 / −0.47417 | −1.82384 / −1.84619 |

The −1 dB limiter was
`alimiter=limit=0.8912509381337456:level=0:latency=1`; the −2 dB candidate
was `alimiter=limit=0.7943282347242815:level=0:latency=1`.
Auto-level is explicitly disabled. There is no `loudnorm` or gain
renormalization. Float limited RMS / LUFS was approximately −4.0103 /
−4.0; decoded −2 dB-limited AAC was approximately −5.0172 / −5.0.
All decoded limited AAC cases had zero samples at or above full scale.
The −1 dB AAC result nonetheless **fails** the stronger −1 dB peak bound.

The actual default decoded AAC all-channel output exceeded full scale:
5.1 / side peaks approximately +4.9041 dBFS, 1,707 / 1,708 over-scale
samples; 7.1 approximately +7.0974 dBFS, 2,077 per side. The full
[AAC receipt](../../tests/fixtures/audio-downmix/aac-results.json) retains
these failures and the unbounded-pan failures beside the limited results.
[Margin receipt](../../tests/fixtures/audio-downmix/margin-results.json)
contains the decoded −2 dB candidate results. AAC has 576,512 decoded
samples versus 576,000 source samples; retained metrics include the actual
decoded padding rather than silently trimming it.

## Reproduce and interpret the receipts

The executed collectors are preserved byte-for-byte under
[scripts/audio-downmix-qualification](../../scripts/audio-downmix-qualification/collector.py).
They are forensic synthetic collectors, not a production media service.
Use a fresh owned empty output root, stage all four scripts there, and run
them with Python 3. The first argument is that absolute root. With
`S09_CONTAINER` set, each media subprocess is `docker exec` into that
already-created isolated container; without it, the exact fixed shipped
paths must exist on the execution host. The output root must have the same
absolute path in the container. Existing outputs are refused, not overwritten.

```bash
S09_CONTAINER=owned-container python3 owned-root/collector.py /absolute/owned-root
S09_CONTAINER=owned-container python3 owned-root/aac.py /absolute/owned-root
S09_CONTAINER=owned-container python3 owned-root/margin.py /absolute/owned-root
S09_CONTAINER=owned-container python3 owned-root/astats.py /absolute/owned-root
python3 -m unittest tests.operations.test_audio_downmix_qualification
```

The first phase measures source, default and named float outputs plus the
−1 dB limiter. The second adds actual `-c:a aac -ac 2 -b:a 160k -ar 48000`
encoding and decoded metrics. The third measures the explicit −2 dB margin
using retained unbounded pan input, not a differently generated fixture.
Every channel is extracted independently with `pan=mono|c0=cN,ebur128`.
RMS / sample peak / full-scale counts are computed from actual decoded
float samples. FFmpeg stderr is retained for every command; `ebur128`'s
final summary supplies integrated LUFS. Its −70 LUFS silence sentinel is
not a measured nonzero source level.

Executed argv receipts are [PCM](../../tests/fixtures/audio-downmix/commands.json),
[AAC](../../tests/fixtures/audio-downmix/aac-commands.json), and
[margin](../../tests/fixtures/audio-downmix/margin-commands.json), and
[astats](../../tests/fixtures/audio-downmix/astats-commands.json).
The fourth phase cross-checks twelve retained all-channel outputs with the
shipped engine's `astats`: unbounded PCM, −1 dB PCM, decoded −1 dB AAC, and
decoded −2 dB AAC for each layout. Its [numeric log excerpt](../../tests/fixtures/audio-downmix/astats-crosscheck.txt)
retains both channels' peak/RMS and zero NaN/Inf counts. Values agree with
the collector to five decimal places; the focused test checks all 24 sides.
All source and PCM output SHA256s are in the PCM receipt. Exact collector
hashes are pinned by the focused test. Remote result file SHA256s before
repository import were:

Repository import adds only a final newline to these four JSON documents;
the focused test verifies the original content digest without that newline.
[AAC artifact hashes](../../tests/fixtures/audio-downmix/aac-artifacts.sha256)
retain every actual encoded output identity, including the margin candidates.

| Receipt | SHA256 |
|---|---|
| PCM | `18b18434509b5a182bd701353dd8972ebb59d159f54d8a6f46b453c08a0baa70` |
| AAC | `c46419f80d20a777eeafdf7bace85fa4e57db28363af38d18952bdfcb5b1f587` |
| Margin | `e27e7a58a07bd70f5eafda5100cf358d1135f932279c0e0fa4141362b361c035` |
| Astats argv | `fd9868c04fbd2373d6ef127829352b061b66059871c537206ae365ad73d4a9f4` |

The focused receipt tests prove source facts, per-side FC measurement,
retained unbounded / insufficient-limiter failures, and the passing synthetic
margin. They do **not** execute FFmpeg again or substitute unit results for
these actual measurements.

## M2 propagation boundary

The accompanying implementation keeps an optional server-owned snapshot next
to legacy scalar fields, rather than rewriting historical stored recipes.
Absent snapshots preserve old producers and identifiers. New claims are
canonicalized independently of the resolved server answer, so source refresh
does not turn an otherwise identical client retry into a different intent.

| Consumer | Typed decision and compatibility rule |
|---|---|
| Rolling/copy/progressive argv | Actual selected-track decision supplies codec/channels/rate; matching legacy AAC-six conversion keeps exact old argv. Progressive GET retains the already-defined bounded flat `achannels` claim; absent retains old 256 kb/s conversion. Reason text is never an argv selector. |
| Encoded VOD | Only AAC encode or no-audio snapshots; the existing AAC-LC/48 kHz/1,024-sample film-global lattice stays unchanged. |
| Recipe/digest/rendition | Audio byte semantics participate; explanation text does not. Existing no-snapshot v4 golden key and `aaction` copy/AAC semantics remain. |
| Master/rung/producer budget | Actual audio codec and negotiated encode rate; copied audio uses existing conservative headroom, not invented probe bitrate. |
| Offline SQLite/replica | Additive nullable snapshot (SQLite v88, replicated v66); preexisting packages stay null. Accepted snapshots survive claims and idempotent retries. |
| Remote/prepared ownership | Actual producer answer survives owner response and recovery; video-only successors inherit it, incompatible changed audio requires reopen. |

These are existing resolver decisions, not authorization to ship the measured
candidate pan/limiter recipes. Main startup snapshots/timing and concurrent
parser-floor extensions must be preserved when porting onto the current
effort. Numeric results above remain attributed to frozen source `8a7dbf533`,
even if that implementation base moves.

## Implementation boundary and remaining whole M4 acceptance

1. Treat the three named matrices plus a measured limiter recipe as candidate
   output identities, not a universal dialogue fix. A `DownmixMatrix` identity
   must include layout, gains and limiter semantics wherever bytes/cache keys
   are propagated. Unmeasured layouts remain unqualified; no count-to-layout
   inference or arbitrary fallback matrix is justified.
2. Preserve encoded VOD's AAC-LC 48 kHz / 1,024-sample film-global lattice,
   source/schema layout facts from #636, AAC-six-channel copy conversion's
   explicit `5.1`, and legacy `aaction` copy / AAC spelling. A limiter's
   lookahead and `latency=1` still need production VOD seek/join regressions;
   synthetic duration alone cannot authorize changing those semantics.
3. Accompanying M2 work carries the existing typed decision through options,
   argv, recipe / digest, manifest / rung totals, durable offline snapshots
   and prepared handoffs. It leaves default downmixing intact: the measured
   pan/limiter candidates above are not authorized production output merely
   because this synthetic subset passed. Final implementation verification
   must describe its actual source tree separately from this frozen receipt.
   No switch or client capability claim is introduced.
4. The original drama, action/LFE, music and 7.1 content comparisons and one
   device's listening notes remain open. Apple AVR / AirPods and Android /
   web route evidence remains open. These generated signals are not invented
   films, listening observations, receiver output, or complete M4 acceptance.
