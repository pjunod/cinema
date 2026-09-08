# M6 axis case results — physical Apple TV admission failed

**Status:** complete · **Outcome:** fail · **Executed:** 2026-09-03 ·
**Baseline:** `c2702f6166ecc89cf0ab43393e556a3d1575ec1e`

Companion to
[M6-AXIS-CASE-HANDOFF.md](M6-AXIS-CASE-HANDOFF.md) — this report records the
required physical-device run of the product transition the fleet actually
produces: 2160p source/direct play to 1080p server-selected transcode.

> **Correction, 2026-09-08 — this report documents the SUPERSEDED attempt, and
> its verdict below is no longer the fleet's.** The run recorded here used a
> 30 Mbit/s shaping proxy (§3) against an 18.183 Mbit/s predecessor: 1.65x
> headroom, *below* the floor `headroom_refusal` itself enforces. So it was
> measuring a transition the server would have declined anyway, which is why
> eight of twenty failed before the swap.
>
> The pair was re-run the same day on a 40 Mbit/s link — 2.20x, above that
> floor — and came back **20/20 clean, zero failed admissions, zero
> predecessor or post-commit stalls**. On that result
> `{ResolutionOrBitrate, DeliveryMethod}` WAS admitted, in commit `0cb370ac`
> "admit the axis pair the hardware run measured", and it is in
> `PREPARED_AXIS_SETS` on `main` today. `a72a1f67` later narrowed it to one
> direction — toward a server-selected successor — because
> `headroom_refusal` computes its floor from the predecessor's rate, which is
> only the conservative side when the successor is a rung the server chose.
>
> **Read §1 as "what a run below the throughput floor looks like", not as the
> admission decision.** The rows here remain useful as bounds and as the
> reason the floor exists.
>
> **The receipt for the run that did admit the pair is not in this folder.**
> It exists only in `0cb370ac`'s commit message. That is a gap: the axis table
> is receipt-driven by design, and the receipt it rests on should be a document
> beside this one. Whoever next runs an axis case should write it up here.

## 1. Verdict — do not widen `PREPARED_AXIS` (superseded, see the correction above)

**The case failed.** The Apple TV completed 12 of 20 attempts cleanly. Eight
attempts failed before the swap because the predecessor did not reach the
chosen film boundary while the successor pipeline was being prepared. The
longest clean sequence was three commits; the acceptance requirement is 20
consecutive clean commits.

The result selects the failure outcome in the handoff:

- Keep `PREPARED_AXIS` unchanged. This run does not admit the measured
  resolution-plus-delivery combination.
- Keep M6 on the existing one-player release-and-replace path. Do not build
  §3.4 on this evidence.
- Keep `MultipleAxes` in force for this transition.
- M8 §10.2 remains dependent on an admitted preparation path and does not move
  forward on this result.

The successful rows are useful bounds, but they do not rescue admission.
Admission is binary because one failed transition is visible playback harm.

## 2. What ran — one product recipe on the required device

### Device and source baseline

| Item | Value |
|---|---|
| Device | Apple TV 4K (3rd generation), `AppleTV14,1` |
| OS | tvOS 26.6 (`23L773`) |
| Cohort run ID | `bb95c189-7949-4717-bac7-846861f2605a` |
| Run window | 2026-09-03 16:28–16:41 UTC |
| Repository baseline | clean detached checkout of `c2702f6166ec` (`origin/main`) |
| Required device coverage | Apple TV complete; optional iPhone not run |

### Recipe pair

| | Predecessor | Successor |
|---|---|---|
| Source identity | `axis-source-2160-v1` | same file, cropped from film time +6 s |
| Delivery | source/direct-play MP4 | server-selected/transcoded HLS |
| Video | H.264 · 3840×2160 · 24 fps | H.264 · 1920×1080 · 24 fps |
| Audio | AAC · 48 kHz · mono | AAC · 48 kHz · mono |
| Grade | SDR | SDR |
| Duration | 132.208 s | 126.208 s |
| Average media rate | 18.183 Mbit/s | 9.626 Mbit/s |

The source MP4 SHA-256 is
`a4b8ba7da6299b3253bf828bd454381a7639f1e5c7f26d0ffe411d8635d55827`.
The successor directory manifest SHA-256 is
`9e3c18d759e8b78f9b129d8b04140f6677226a6bdc1f1f41fedd4eaacacb9bfa`.

### Harness and network

The disposable tvOS harness held two live `AVPlayer` pipelines. It started the
2160p direct-play predecessor 15 s before the selected film boundary, prepared
and prerolled the 1080p HLS successor, acknowledged only after at least 12 s of
corrected contiguous successor runway, then swapped `AVPlayerLayer`s at the
boundary. Commit boundaries were encoded 24 fps frame boundaries from 30.0 s
through 33.5 s. AVFoundation received 3 s between trials to retire both
pipelines.

The link used one stable, shared 30 Mbit/s shaping proxy with no bandwidth
cliff. Unique run/trial/pipeline URLs let the proxy count actual response-body
bytes separately for both pipelines.

**Difference from M6:** this was a local client-side layer swap. It did not
implement an `action_id`, reserve/prime/commit phases, a server-side prepared
session, or any M6 transaction behavior. Its runway numbers describe this
harness switch, not an unbuilt M6 switch.

## 3. Acceptance — four supporting fields passed, admission did not

| Requirement | Result | Evidence |
|---|---|---|
| 20 consecutive clean commits; zero predecessor and post-commit stalls | **FAIL** | 12 clean commits · 8 failed admissions · longest clean sequence 3 |
| Runway varies; `full at ack` is false every time | Pass | 12,000–13,500 ms · 4 distinct values · 0/20 full |
| Every copied buffer maps at or beyond the boundary | Pass on completed commits | 12/12 qualified · 0.0 ms position error; failed admissions never committed or copied a post-commit frame |
| Wire counts vary by trial | Pass | predecessor: 20/20 distinct · successor: 15/20 distinct on the required device |
| Memory and hardware decoder count declared unanswered | Pass | both were left null with the instrument limits recorded below |

**How to read this:** the zero `waitingToPlay` counters do not turn the eight
failed admissions into clean commits. In those attempts the predecessor still
failed to reach its boundary before the 25 s boundary watchdog, so no swap was
made. A clock that never declares `waitingToPlay` is not evidence that it made
forward progress.

## 4. Trial receipts — 12 clean, 8 rejected before commit

Wire values are response-body megabits delivered by the acknowledgement
snapshot, not asset-size estimates. A dash means the trial never committed, so
there was no successor frame or post-commit observation to report.

| # | Result | Boundary s | Runway ms | Full | Film PTS s | Frame ms | Position ms | Pred wire Mbit | Succ wire Mbit | Pred stalls | Post stalls |
|---:|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | pass | 30.0 | 12,000 | false | 30.0 | 6.35 | 0.0 | 541.20 | 289.88 | 0 | 0 |
| 2 | pass | 30.5 | 13,500 | false | 30.5 | 2.83 | 0.0 | 514.06 | 310.90 | 0 | 0 |
| 3 | **fail** | 31.0 | 13,000 | false | — | — | — | 362.15 | 288.55 | 0 | — |
| 4 | pass | 31.5 | 12,500 | false | 31.5 | 4.58 | 0.0 | 461.64 | 288.81 | 0 | 0 |
| 5 | pass | 32.0 | 12,000 | false | 32.0 | 11.10 | 0.0 | 420.87 | 289.08 | 0 | 0 |
| 6 | pass | 32.5 | 13,500 | false | 32.5 | 9.12 | 0.0 | 488.90 | 306.16 | 0 | 0 |
| 7 | **fail** | 33.0 | 13,000 | false | — | — | — | 350.62 | 287.78 | 0 | — |
| 8 | **fail** | 33.5 | 12,500 | false | — | — | — | 392.95 | 288.43 | 0 | — |
| 9 | pass | 30.0 | 12,000 | false | 30.0 | 9.22 | 0.0 | 416.42 | 289.88 | 0 | 0 |
| 10 | pass | 30.5 | 13,500 | false | 30.5 | 8.45 | 0.0 | 523.24 | 310.12 | 0 | 0 |
| 11 | **fail** | 31.0 | 13,000 | false | — | — | — | 348.26 | 288.81 | 0 | — |
| 12 | **fail** | 31.5 | 12,500 | false | — | — | — | 381.16 | 288.29 | 0 | — |
| 13 | pass | 32.0 | 12,000 | false | 32.0 | 6.55 | 0.0 | 463.34 | 289.21 | 0 | 0 |
| 14 | pass | 32.5 | 13,500 | false | 32.5 | 14.24 | 0.0 | 474.87 | 306.03 | 0 | 0 |
| 15 | **fail** | 33.0 | 13,000 | false | — | — | — | 371.33 | 288.17 | 0 | — |
| 16 | **fail** | 33.5 | 12,500 | false | — | — | — | 398.59 | 287.91 | 0 | — |
| 17 | pass | 30.0 | 12,000 | false | 30.0 | 6.35 | 0.0 | 421.00 | 289.88 | 0 | 0 |
| 18 | pass | 30.5 | 13,500 | false | 30.5 | 2.97 | 0.0 | 496.24 | 310.12 | 0 | 0 |
| 19 | **fail** | 31.0 | 13,000 | false | — | — | — | 357.17 | 288.81 | 0 | — |
| 20 | pass | 31.5 | 12,500 | false | 31.5 | 1.58 | 0.0 | 435.42 | 289.93 | 0 | 0 |

Every failed row recorded the same error:
`predecessor did not reach commit boundary`.

## 5. Field summaries — what the numbers do and do not say

| Field | Result | Interpretation |
|---|---|---|
| Corrected contiguous runway | min 12,000 ms · max 13,500 ms · mean 12,750 ms | Varied across the 132 s fixture; no max/sum over disjoint loaded ranges |
| `full at ack` | false in 20/20 | The run did not reproduce the short-fixture full-buffer artifact |
| First-frame latency | 1.58–14.24 ms · mean 6.94 ms on 12 commits | Measured from commit to copied, boundary-qualified pixel buffer |
| Switch position error | 0.0 ms on 12 commits | Copied film PTS equaled the frame-aligned commit boundary |
| Predecessor wire | 348.26–541.20 Mbit · mean 430.97 Mbit | 20 distinct acknowledgement snapshots |
| Successor wire | 287.78–310.90 Mbit · mean 293.84 Mbit | 15 distinct acknowledgement snapshots |
| Total wire | 637.07–833.36 Mbit · mean 724.81 Mbit | Both pipelines contended under one stable link cap |
| Predecessor `waitingToPlay` transitions | 0 | Does not override the eight boundary-progress failures |
| Post-commit `waitingToPlay` transitions | 0 on 12 commits | No post-commit stall was observed where a commit occurred |
| Peak memory | unanswered | Available tooling does not expose resident memory for `mediaplaybackd` / `videocodecd`; app RSS would measure the wrong process |
| Hardware decoder instances | unanswered | AVFoundation exposes no public hardware-decoder identity; the harness proved two logical `AVPlayer` pipelines only |

Plan §5.3's one-slot decoder question therefore remains open. This run must not
be cited as evidence for either one or two hardware decoder instances.

## 6. Instrument admission — shakedowns were excluded

Instrument shakedowns ran before the official run and are not mixed into the
20 receipts above:

1. The first video-output probe could begin polling after playback began and
   miss the first decoded frame. It was armed before `playImmediately`.
2. Commit boundaries were aligned to encoded 24 fps frames. This prevents an
   arbitrary sub-frame timestamp from masquerading as a missing output frame.
3. A four-trial shakedown exposed an 18 s boundary watchdog and 450 ms player
   reuse cadence as potential harness confounds. The admitted cohort relaxed
   them to a 25 s watchdog and 3 s teardown interval.

The final cohort still failed eight times after all three corrections. No
failed official row was discarded or rerun.

## 7. Evidence and cleanup — production state was restored

The shaping proxy persisted the authoritative JSON receipt before teardown:

```text
SHA-256  8f764ad577ab15b8d4318443964ebe7e23880e8ea16f23aed2a060b98df8f4e6
Run ID   bb95c189-7949-4717-bac7-846861f2605a
Rows     20
Result   12 passed / 8 failed
```

The full row data needed to review the decision is reproduced in §4, so this
document remains useful after the disposable raw file and harness are removed.

No production source, server behavior, or `PREPARED_AXIS` rule was changed.
After the run, the disposable harness was replaced on the Apple TV with Noirr
Cinema v0.3.0 build 115, and that production app was relaunched successfully.

## 8. Limits — this is the required SDR case, not every Apple case

- The run covers the required Apple TV 4K (3rd generation) and the SDR H.264
  resolution-plus-delivery product. The optional iPhone cohort was not run.
- The harder HDR/Dolby Vision grade-change variant was not run. Its absence
  cannot improve the failed admission and does not weaken the failure result.
- The deterministic fixture was locally generated. The successor was
  pre-transcoded from the same source and labeled `server_selected`; the test
  deliberately did not exercise a server transaction, per the handoff's
  non-goal.
- The result diagnoses capability admission, not the root cause of the
  predecessor progress failures. A separate diagnostic effort would be needed
  to distinguish decoder pressure, AVFoundation scheduling, and other device
  resource effects.
