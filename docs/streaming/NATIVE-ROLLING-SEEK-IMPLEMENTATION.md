# Native rolling seeks — successor dependencies and measured reach

**Status:** implementation in progress; native latency qualification open ·
**Written:** 2026-10-03 EDT · **Base:** `f48538900`, `effort/web-seek`.

Companion to [the reviewed seek RCA](WEB-SEEK-LATENCY-RCA-AND-FIX.md),
[its Opus review](../reviews/WEB-SEEK-LATENCY-OPUS-REVIEW.md), and
[the development pipeline](../DEVELOPMENT_PIPELINE.md). This record owns
native server successor dependencies and a separate native Safari experiment.
The parallel web task owns loader, frame, control, catalog and shared RCA
changes. Nothing here authorizes deployment or claims incident closure.

## What the implementation changes

An explicitly selected copy-audio track no longer waits for language defaults
that cannot influence the selected track. Copy does not burn subtitles.
Unspecified audio retains the existing saved-language and dual-audio anime
selection. Negative sentinels retain their prior meaning; validation and the
copy argument builder still own their interpretation.

Pacing reads `HLS_READRATE` and `HLS_BURST_SECS` in one committed settings
snapshot. Previously they used two serial consensus reads, which could also
combine different committed policies. Missing, malformed, negative and NaN
values retain the old defaults; zero and capability degradation retain the
existing behavior.

| Create path | Previous settings reads | Candidate settings reads |
|---|---:|---:|
| Explicit copy audio plus pacing | 3 | 1 |
| Automatic copy audio plus pacing | 3 | 2 |

This counts these dependencies only, not all create/admission/catalog work.
The [copy start owner](../../crates/plurxd/src/transcode/manager/start.rs)
and [pacing owner](../../crates/plurxd/src/transcode/manager/describe.rs)
log monotonic `copy_track_selection` and `pacing_policy` durations through
existing startup-phase logging. This makes actual savings measurable. Two
avoided reads are not evidence that a 6.5–9.5 s native reopen now takes less
than 2 s.

## The shorter-GOP experiment uses actual writer cuts

The ignored `copyseg::tests::native_seek_short_gop_media_export` test exports
900 s of generated AVC/AAC media through the real copy pipe and
[GOP-aware segmenter](../../crates/plurxd/src/copyseg.rs). The two arms use
the same source and output recipe, with deliberately different lab limits:

| Arm | Floor | Preferred ceiling | Strict covering target |
|---|---:|---:|---:|
| Shipped copy policy | 6 s | 15 s | 16 s |
| Short-GOP lab policy | 2 s | 2 s | 3 s |

This changes actual segmentation, rather than relabeling a 16 s output.
The exporter checks every advertised duration and compares every decoded
video frame hash against the source. Concatenated decoding does not prove
independent random-access presentation of every individual segment. It cannot
prove the incident source's
GOPs, HEVC leading-picture behavior, production admission, or source identity.
The strict-bound branch currently counts a preemptive cut as a ceiling cut
regardless of whether its next keyframe is clean; decoded-frame comparison
provides the fidelity evidence for this AVC fixture.

The [loopback fixture](../../tests/playback/native-rolling-seek-fixture.py)
pairs served edge/revision with native browser `buffered` and `seekable` at
attachment and every 500 ms. It records playlist requests/reload cadence,
actual durations, playback rate, committed input, `seeked`, target media time,
click-to-frame latency, landing error and outside-reach outcomes. The native
guard remains: an unreachable destination is recorded, never assigned as a
local seek and credited as success.

The fixture models a 48 s allowance, an 8× initial burst through 48 s and
2× production afterward. It uses the actual segmenter bytes but a modeled
publication/flow layer. It does **not** exercise daemon scratch grants,
concurrent streams, generation/authority fences, reattachment or successor
creation. Its requests identify what Safari was served separately from the
observer's most recent served snapshot. A sample cannot assume Safari has
already consumed the observer's revision.

```bash
# Generate actual copyseg output under both explicit lab policies.
PLURX_NATIVE_SEEK_MEDIA=/tmp/plurx-native-seek-media \
  rustup run 1.97.1 cargo test -p plurxd --bin plurxd \
  native_seek_short_gop_media_export -- --ignored --nocapture

# Loopback only; open this URL in a separate Safari tab using CUA.
python3 tests/playback/native-rolling-seek-fixture.py \
  --media /tmp/plurx-native-seek-media \
  --receipt /tmp/plurx-native-seek-safari.json --port 8767
```

Start each arm, capture the first attachment, run twenty reach attempts and
five spaced +30 presses, and observe a full ten-minute refill interval.
Record failures and outside-reach attempts in the denominator. The fixture
never silently replaces an attachment to hide a reach failure. Pause, resume
and 2× controls support separate rate/intent rows.

## Production reach remains a separate contract

The production target remains 16 s and the initial reserve remains 48 s.
No scratch cap or reserve has been widened, no source has been forced through
an encoder, and no shorter source cadence has been inferred from the browser
buffer. The previous 108 s reserve trial and the 128 s steady +30 estimate
cannot be made safe by treating their arithmetic as qualification.

A complete source/GOP plan is missing on the incident's rolling fallback
route. A shorter contract chosen from the initial GOPs cannot prove a later
long GOP will fit. Rejecting that later GOP would terminate a previously
playable stream; changing the target after publication would break the frozen
presentation contract. Even an encoded-only shortcut needs an explicitly
qualified encoder/CFR contract, not `-hls_time 2` alone: force-keyframe behavior
and VFR rounding remain relevant.

A production shorter-target follow-on must freeze the covering target into
the presentation identity before the first response, validate every segment
and audio tail, retain compatible retry contracts, and use that same target
for publication cadence. Keep the existing scratch/retention accounting,
including complete in-flight segments and exact write grants. Qualify source
GOP boundaries and leading-picture fidelity on the reference sources.

Reusing predecessor scratch needs retained-file, generation, recipe, achieved
timeline and current authority proof. Browser buffer coverage proves none of
these. This implementation does not introduce a reuse path without them.

Required acceptance remains the RCA's native rows: a measured bounded
successor or maintained +10/+30 reach at attachment and refill low point,
five spaced and five coalesced presses, exact landing/frame evidence,
1×/2×, pause/resume, concurrent streams, scratch pressure, and reference-fleet
p95 below 2 s for unavoidable replacements. Generated fixtures supplement
those rows; they cannot close them.

## Validation and review

Rust 1.97.1 was verified locally before editing. Baseline daemon all-target
check passed. The 18 focused `rolling_publication_budget` tests passed; they
cover existing allowance, pacing, low reserve, EOF and authority constraints.
No production publication/flow constants were changed.

The focused successor test
`transcode::tests::native_successor_settings_preserve_track_policy_and_pacing_defaults`
checks explicit audio, saved-language fallback, automatic Japanese selection,
malformed policy defaults, zero pace and both copy/transcode capability
resolution. It checks behavior; source review proves the read-count reduction.

Independent adversarial review found no correctness blocker in the lookup
changes and required stronger automatic-audio coverage, honest read-count
claims, and retention of source/future-GOP qualification limits. The test now
uses multiple tracks and independently expected language/anime selections.
The reviewer reproduced a fixture receipt race (48 s transmitted but 56 s
logged) and found cross-run asynchronous sampling and paused-frame hazards.
Receipts now capture the exact transmitted snapshot, asynchronous work is
fenced to its run, and destination frames arriving before `seeked` are retained.
Frame subscriptions are cancelled and rearmed on attachment replacement.
The actual-script Node counterexamples and Python payload-boundary regression
pass:

```bash
node tests/playback/native-rolling-seek-fixture.test.js
python3 -m unittest tests.operations.test_native_rolling_seek_fixture
```

The real copyseg exporter passed on 2026-10-03: baseline 151 segments,
2.021375–6.000000 s; short arm 450 segments, 2.000000–2.021375 s.
All copied video frame hashes matched the source in both arms. Daemon
all-target check, denied-lint Clippy, the successor regression, documentation
index and validation catalog checks passed locally with the pinned compiler.
The Mac linker emitted its existing large-unwind-table warning on the test
binary; no Rust denied lint failed.

Physical Safari evidence, exact-head compile results and the task PR/gate
receipt will be added here when available. CUA reported that the Mac was
locked during the first test attempt; the user was asked to unlock it. Apple
hardware availability is not an inferred blocker.
