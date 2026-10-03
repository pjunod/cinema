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

The incident rolling fallback did not load a compatible complete source/GOP
plan. Existing artifacts alone do not prove compatibility with that request. A shorter contract chosen from the initial GOPs cannot prove a later
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

## What the exact incident source shows

Read-only inspection of `m6` on 2026-10-03 identified file `5208`,
“Bad Boys: Ride or Die”, at the catalog path
`/media/movies/Bad Boys Ride or Die (2024)/Bad Boys Ride or Die (2024) Remux-2160p.mkv`.
The source metadata is 79,519,453,096 bytes with mtime `1727490599`.
No playback, settings, analysis build or deployment was initiated.

Bounded `ffprobe -show_packets` reads at `0%+120` and `3300%+120` report
packet keyframe gaps no greater than 0.876 s. This is not the clean-cut
contract. Two existing 244,142-byte video-only artifacts match the catalog
size/mtime and their blob SHA-256 matches the replicated artifact row. Both
use segment-plan version 3 and describe the same 6,958.952 s video timeline:

| Indexed fact | Value | Meaning |
|---|---:|---|
| Fragment rows | 10,149 | Source pipe fragments, not served HLS segments |
| Maximum fragment duration | 0.960 s | Does not bound gaps between usable clean cuts |
| `CleanIdr` rows | 3,690 | Classified clean starts in this indexed recipe |
| `Dirty` rows | 6,459 | Includes conservatively rejected non-IRAP openings |
| First clean-cut gap | 38.914 s | Frequent packet keyframes do not supply early clean starts |
| Maximum clean-cut gap | 169.920 s | At 6,743.737–6,913.657 s; incompatible with a three-second clean-only cadence |
| `parameter_sets_constant` | `true` | Configuration stability, not random-access availability |

The artifact keys are
`ad46d6a6c17a59b395a26c69b7066b4b8458c2e46d906f79dc7742094590feae`
and
`a89f7d8cb73d4cd78d83d0c1459a8f0dded6949f084e6315a1be0c005c99529e`;
their verified blob digests are respectively
`f58371adbb3103eac9bedfe17eb42c17ab8a5833114ee7076b390312cb20d240`
and
`e762729cd6d1c2b1c6704eff21a6d8f579ff6198383166dbfde6b6b43a457a19`.
The source content digest recorded in both artifacts is
`d388cb2abd20e58eb23a33b7ce4f8a7433e78c23140773f3070770c1c577a127`.
Size/mtime matching is not a fresh content attestation. Current native-request
recipe compatibility and retained-file/authority reuse remain unverified.

How to read this evidence: neither short pipe fragments nor packet keyframe
flags qualify a three-second clean-copy policy. A strict ceiling cut and its
leading-picture treatment need separate frame-fidelity qualification. The
adversarial reviewer confirmed the class interpretation and these limits;
no production policy change follows from this inspection.

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

The candidate is reviewable as [draft task PR #770](http://192.168.4.7:3000/noirr/plurx/pulls/770)
into `effort/web-seek`. The pinned all-target check and focused regression
also passed against committed `fac7c75e31`.

[Effort gate run 3962](http://192.168.4.7:3000/noirr/plurx/actions/runs/3962)
failed the validation-unit preflight because the ignored exporter added five
process ownership census deltas without their ledger notes. The exporter
children are reviewed and the ledger is corrected in the follow-up commit;
all 253 local validation contract tests passed (one platform skip). This failure
is blocking until the new candidate passes the gate.

The local operations suite ran 585 cases. Its exporter ignore census was
updated from 20 to 21; all ten reasoned-ignore contract tests then passed.
This exporter has already passed explicitly and is not a known-red waiver.
The unrelated hung-Docker janitor fixture took 60 s on this Mac because GNU
`timeout` is absent; the script requires the Linux timeout branch for that
case. Validate that unchanged fixture from committed source on Linux rather
than changing production janitor behavior for this task.

Physical Safari evidence remains open. CUA reported that the Mac was
locked during the first test attempt; the user was asked to unlock it. Apple
hardware availability is not an inferred blocker.
