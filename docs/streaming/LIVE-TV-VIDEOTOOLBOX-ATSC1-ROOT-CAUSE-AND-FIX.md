# ATSC 1.0 on VideoToolbox — caption failure, reviewed repair and evidence

**Status:** open; current-main implementation in an independent agent clone,
final review and validation pending · **Updated:** 2026-09-16 · **Base:**
`c9e4edf45` · **Branch:** `codex/live-tv-videotoolbox-repair`.

Companion to [PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) and the
[status page](LIVE-TV-VIDEOTOOLBOX-STATUS.html). This supersedes the initial
candidate on `10f2afe6`. The old checkout's code and test counts are historical
only; they are not evidence for this branch. The supplied
[Fable review](../reviews/LIVE-TV-VIDEOTOOLBOX-ATSC1-FABLE-REVIEW.md) identified
four blockers, addressed below. The user's latest workflow batches the
functional repair and corrected diagnostics as separate commits in one PR.

## 1. Decision and scope

Disable A/53 caption forwarding with `-a53cc 0` only when Live TV encodes
video with VideoToolbox. Keep hardware video encoding and the existing
public error code. Record recognized encoder failures in session state and
include their sanitized cause in the existing session-end log.

Copy routes, other encoders and VOD retain their existing caption policy.
This creates a real host-dependent difference: Linux hardware/software
encoders that forward A/53 may supply captions to capable web/Android players,
while this Mac live-encode route deliberately drops them. The product's lack
of supported caption controls does not erase that behavioral tradeoff.

The fix bypasses FFmpeg's reproduced SEI insertion failure; it does not repair
FFmpeg's parser or implement caption playback. The same H.264 VideoToolbox
failure also affects caption-bearing files, including preserved DVR captures;
that separate policy decision is tracked in the
[VOD follow-up](VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md).

## 2. Incident — what the affected installation reported

The following evidence was supplied by the user from Claude's investigation
on the affected machine. The original broadcast recording has **not** been
obtained or replayed in this checkout.

| Item | Reported value |
|---|---|
| Server | plurxd 0.3.0, `v0.3.0-2059-gefd54247` |
| Host | macOS 26.6.2, arm64; exact Mac model not supplied |
| FFmpeg | `8.1.2-Jellyfin`, installed under the user's local plurx-ffmpeg directory |
| Input | HDHomeRun channel 5.1, 720p59.94 MPEG-2 with AC-3 audio |
| Audio observation | 5.1/stereo reported; the reproduction converted audio to stereo AAC |
| Symptom | ATSC 1.0 start fails with HTTP 502 and `stream_failed`; observed ATSC 3.0 playback works |
| Caption observation | All 77 inspected decoded frames carried `ATSC A53 Part 4 Closed Captions` side data |

Reported FFmpeg diagnostics:

```text
[h264_videotoolbox] Unexpected end of SEI NAL Unit parsing type.
[h264_videotoolbox] Error copying packet data: -1094995529
[vost#0:0/h264_videotoolbox] Error submitting video frame to the encoder
```

The FFmpeg error is `AVERROR_INVALIDDATA`; the reported child exit was 183.
The report compared the same three-second capture across these commands:

| Encoder/variation | Exit | Segments reported |
|---|---:|---:|
| `h264_videotoolbox`, default caption handling | 183 | 1 |
| VideoToolbox, keyframe forcing removed | 183 | 1 |
| VideoToolbox, minimal encoding arguments | 183 | 1 |
| `libx264`, otherwise identical arguments | 0 | 3 |
| VideoToolbox with `-a53cc 0` | 0 | 3 |

**How to read this:** publishing a first segment does not establish success.
The encoder can produce some output and then terminate while processing a
caption-bearing frame. The decisive comparison changes caption forwarding
while retaining VideoToolbox.

## 3. Independent reproduction — captions distinguish failure from success

The local investigation used macOS 27.0, Homebrew FFmpeg 9.0.1 and the
repository-pinned Rust 1.97.1. The checkout base was
`10f2afe60b3d177866fdcc5741acd9f494525d73`, with existing uncommitted work.
These are historical root-cause measurements, not validation of the rebased candidate.

### 3.1 Controls and the missing fixture property

Initial synthetic MPEG-2/AC-3 samples without captions encoded successfully.
The 1080i route also passed with the production environment allowlist and
720p scaling; its output segment decoded successfully. These controls ruled
out a blanket inability to decode MPEG-2 or encode deinterlaced frames on
this host. They did not exercise the eventual cause.

A sandboxed encoder probe initially failed with compression-session status
`-12908`, including for a basic progressive synthetic input. The same command
passed with access to the host encoder. That environment failure is separate
from the caption-triggered exit 183 and is not evidence of an ATSC defect.

An early four-second fixture was also too short to reliably satisfy the
test's requirement for two published segments before EOF. The retained test
uses eight seconds of media and keeps stdin open until publication. A
separate paced, caption-free input produced its first playlist before EOF;
neither of these fixture observations establishes a separate product bug.

### 3.2 Adding A/53 data reproduced the encoder failure

The reproducer generated four seconds of 1280×720 MPEG-2 at 60000/1001 fps,
inserted A/53 `GA94` user data before each picture's first slice, and remuxed
the video with generated AC-3 audio into MPEG-TS. FFprobe confirmed A/53 side
data on **240 of 240 decoded frames**.

| Local control | Observed result |
|---|---|
| VideoToolbox with its default caption option | Exit 183; SEI parsing error and `Error copying packet data: -1094995529` |
| Same input and encoder, adding only `-a53cc 0` | Exit 0 |

The local message said `parsing size` instead of the report's `parsing type`.
Both are failures in the same SEI insertion/parser path. Reproduction on
FFmpeg 9.0.1 means this cannot be treated solely as a Jellyfin 8.1.2 packaging
problem or solved by recommending an unqualified version upgrade.

The local encoder help independently confirmed:

```text
-a53cc <boolean> Use A53 Closed Captions (if available) (default true)
```

The [FFmpeg VideoToolbox encoder source](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/videotoolboxenc.c)
contains the default-enabled `a53cc` option and the matching SEI error paths
in `find_sei_end`. This upstream link is supporting context and may move;
the command results above are the binary-level evidence. We have not isolated
the exact malformed intermediate NAL or proved a specific off-by-one error
inside FFmpeg.

## 4. Root cause — successful decode followed by failing caption insertion

The production path in [live_tv.rs](../../crates/plurxd/src/live_tv.rs) is:

```text
 HDHomeRun MPEG-TS
        │
        ▼
 retained source prefix → FFprobe → live delivery plan
        │
        ▼
 FFmpeg software MPEG-2 decoder (-hwaccel none)
        │
        ├── video frames
        └── A/53 caption frame side data
                    │
                    ▼
      deinterlace/scale/pixel-format filters
                    │
                    ▼
          h264_videotoolbox encoder
                    │
           a53cc defaults to true
                    │
                    ▼
       caption insertion into H.264 SEI
                    │
                    ▼
     invalid-data error → FFmpeg termination
                    │
                    ▼
        StreamFailed → HTTP 502 stream_failed
```

`live_ffmpeg_command` explicitly selects software decoding with
`-hwaccel none`. It then chooses the output encoder independently. The
failure therefore does not imply a missing VideoToolbox MPEG-2 decoder.

The existing `-sn` and `-dn` arguments do not solve this problem. Captions
arrive attached to decoded video frames; disabling separate subtitle and
data streams does not remove that video-frame side data. Before the candidate
patch, the live command did not override the encoder's caption option.

The evidence also distinguishes the failure from an AC-3 decoding problem,
the forced-keyframe policy and a general interlacing problem: the reported
source was progressive, removing keyframe forcing still failed, and the
caption-only switch resolved the encode.

### 4.1 Why the reported ATSC 3.0 route worked

The supplied report says that its ATSC 3.0 route copied video and carried
captions separately as IMSC1/TTML. A video-copy route does not invoke the
failing VideoToolbox encoder. That is sufficient to explain the observed
difference without claiming every ATSC 3.0 service uses the same route.

The [delivery planner](../../crates/plurxd/src/live_tv_delivery.rs) decides
copy versus encode from source facts, player capabilities and quality limits.
Do not infer that all ATSC 3.0 always copies, that every ATSC 1.0 frame always
has captions, or that every Mac/FFmpeg version must fail. The trigger is the
caption-bearing input reaching this encoder path on an affected combination.

## 5. Current-main encoding contract

The [live command](../../crates/plurxd/src/live_tv.rs) on this base uses
`live_ffmpeg_command_for_input` for both `LiveTvFfmpegInput::Tuner` and
`LiveTvFfmpegInput::GraphProbe`. Production and readiness therefore need
**one** call to `live_caption_args` in the `LiveTrackAction::Encode` arm,
immediately after `encoder.encode_args(...)`.

```rust
fn live_caption_args(encoder: Encoder) -> &'static [&'static str] {
    match encoder {
        Encoder::VideoToolbox => &["-a53cc", "0"],
        _ => &[],
    }
}
```

| Route | Required behavior |
|---|---|
| VideoToolbox live encoding and graph probe | One `-a53cc 0` option |
| Software, QSV, VAAPI, NVENC live encoding | No new caption option |
| Video copy with copied or converted audio | No encoder caption option |
| VOD/shared encoder builder | Unchanged in this PR |

Do not restore the earlier `append_live_hls_output` extraction, duplicate
probe call site, fake-FFprobe setup or keyframe-order changes. Main already
contains the relevant changes in another shape. Its one-second HLS cadence
and 24-entry playlist stay intact. `spawn_live_ffmpeg` returns `(Child,
ChildJob)`; the hardware test retains the job until the child is reaped.

## 6. Diagnostics — a session fact, not another warning

The first candidate's standalone warning and logging-only substring test
were rejected. The updated design carries
`Arc<StdMutex<Option<&'static str>>>` beside `decoder_unavailable` on the
session. `capture_live_stderr` records a recognized static encoder cause,
including when the diagnostic arrives with the initial descriptor or across
read boundaries. Only the first recognized cause is retained. Raw tuner
metadata is never stored as the diagnostic.

After cleanup joins the stderr reader, `classify_live_source_error` enriches
an existing `StreamFailed` message with that cause while preserving the
`stream_failed` code. It does not set `decoder_unavailable`, relabel an
encoder problem as missing codec support, or turn cancellation into failure.
The same message then reaches startup errors, `state.error` and the terminal
error. Apple still uses its code-based UI; changing that is outside this PR.

`retire_session` already logs `Live TV session ended` at info level with
channel, user, reason, duration and input-byte count. Add the sanitized cause
there. Do not add a warning for ordinary idle/provisional expiry or duplicate
that terminal event. The existing sanitizer caps output and hides HTTP(S)
URLs; it is not a universal secret-redaction guarantee.

Tests must drive `capture_live_stderr`, inspect the resulting fact and
actual decoder flag, then call the classifier. Required cases: parsing-type
and parsing-size diagnostics, a boundary split across 1,024-byte reads, the
initial descriptor chunk, repeated diagnostics/first-cause retention, an
unrelated error, and normal cancellation. Mutating the parser to set the
decoder flag or stop feeding the diagnostic window must fail those tests.

## 7. Hardware regression and validation contract

The [hardware test](../../crates/plurxd/src/live_tv/videotoolbox_tests.rs)
generates MPEG-2/AC-3 with A/53 data on every decoded video frame, verifies
that with FFprobe, and drives the real source probe, delivery plan and live
command. It requires two playlist entries before stdin closes; that proves
publication without EOF flushing, not sustained real-time playback.

| Fixture | Audio input | Encoded output |
|---|---|---|
| 720×480 interlaced, SAR 8:9 | AC-3 stereo | 480p H.264 / stereo AAC |
| 1280×720 progressive, 60000/1001 fps | AC-3 5.1 | 720p H.264 / stereo AAC |
| 1920×1080 interlaced | AC-3 stereo | 1080p H.264 / stereo AAC |
| 1920×1080 interlaced | AC-3 stereo | 720p H.264 / stereo AAC |

Main's one-second cadence can leave the finite fixture's final segment with
one video frame and no audio packets. Decode video with `-xerror` in every
listed segment. Require actual AAC audio packets and decode audio in all
non-final segments; decode final-segment audio when packets exist. Do not
weaken checks on earlier segments to accommodate the tail.

On an input write failure or publication timeout, close stdin, collect the
child's status and stderr, and only then fail the assertion. A bare Broken
pipe must not hide the SEI error. Overall process time remains bounded.

**Evidence status:** final candidate checks are pending. The old base's
75-test count and 10.75-second hardware run are superseded. Fable observed
134 live-TV tests on its hand-port and exposed the tail-segment failure.
Neither count substitutes for results against this branch's final SHA.

The repository already pins Rust 1.97.1. The shell trap is selecting
Homebrew cargo/rustc, which do not honor Rustup's selection. Put Rustup's
binaries first or invoke `rustup run 1.97.1`; verify `rustc --version`.

```bash
rustup run 1.97.1 rustc --version
# At final validation, after the adversarial review:
rustup run 1.97.1 cargo test -p plurxd --bin plurxd live_caption --locked
rustup run 1.97.1 cargo test -p plurxd --bin plurxd live_tv:: --locked
rustup run 1.97.1 cargo test -p plurxd --bin plurxd \
  live_tv_videotoolbox_atsc1 --locked -- --ignored --nocapture
python3 tests/operations/test_docs_index.py
```

Set `PLURX_FFMPEG` and `PLURX_FFPROBE` for another build. The hardware test
requires host VideoToolbox access; an ordinary test run ignores it and does
not prove hardware acceptance. Compiler/lint checks run during development;
per the user's 2026-09-16 instruction, tests wait for final review and the
fast lane. Broad unrelated unit-suite repair belongs to another process.

## 8. Fable review disposition

| Finding | Resolution |
|---|---|
| B1 stale base | Port only the intended changes onto `c9e4edf45` in an independent clone; one shared builder call; retain `ChildJob`. Recheck main before final review. |
| B2 tail segment | Count packets; always decode video; require/decode audio on all non-final segments; allow an audio-free final tail. |
| B3 duplicate/noisy warning | Drop the added warning; enrich the existing info-level terminal event. |
| B4 untested recognizer | Store a session fact, enrich the API/session error, and test through the actual stderr reader and classifier. |
| F1 DVR/VOD | Open a separate follow-up with reproduced scope and acceptance criteria. No VOD policy change in this repair. |
| F2 caption asymmetry | Explicitly describe Mac versus forwarding Linux routes in §1. |
| F3 fixture | Retain the reviewed `GA94` packet and non-empty/all-frames checks. |
| F4 5.1 and before-EOF limits | Add AC-3 six-channel input to the reported 720p shape; retain the bounded before-EOF check with its stated limits. |
| F5 masked stderr | Return input errors from the feeding task and await child output before asserting. |
| F6 stale docs/toolchain | Rewrite current implementation and evidence sections; explain the Homebrew/Rustup distinction. |

The supplied review is the basis for these corrections. The user's later
instruction asks for an adversarial agent review only when the batched PR is
ready to merge; conduct that final review once, address its findings, then
run the fast lane. Do not repeatedly request re-reviews.

## 9. Release acceptance and boundaries

Commit the encoding and diagnostic changes separately, batch them in one
draft PR, finish the status/documentation work, then review the final diff.
After addressing review findings, run the focused final checks, real Mac
hardware test and fast lane. Fix any failures; merge only the current green
candidate. Record the exact tested commit and PR on the status page.

Deployment is a separate process. The original affected Mac with Jellyfin
FFmpeg 8.1.2 still needs repeated channel starts and playback across HLS
window rollovers; generated fixtures are not that household acceptance.
Selecting software encoding is a temporary workaround with CPU/concurrency
cost, not a claim that the hardware path was repaired on an untested machine.

Non-goals: caption rendering, FFmpeg parser backports, generic fallback/retry,
VOD caption policy, Apple error UI, HLS timing, tuner discovery and cluster
ownership. No feature gate or enable toggle is needed for this bug fix.
