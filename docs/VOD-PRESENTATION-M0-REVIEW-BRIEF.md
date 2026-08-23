# VOD M0 review brief — three normative sentences to re-decide

**Status:** RULED 2026-08-23 — A1, A2 and A3 decided, M1 authorized with
amendments, D6 left open; the amendments are applied to plan §2.1, §2.2, §2.3,
§6 and §9, and §12.9 records them. The sections below are kept as the record of
what was asked and on what evidence · **Ruling summary:** A1 — adopt
byte-landing identification, strengthened to a 3-fragment byte-count sequence,
typed failure on no match or a double match; the `tfdt` rebase is untouched.
A2 — documentation, not policy: `TARGETDURATION` bounds time, not bytes, and
M4 sizes the web segment budget from `est_bytes`. A3 — write the invariant:
`block_budget_secs` = the client's own configured first-byte timeout minus 2 s,
plus a new `playback.vod_materialize_budget` (30 s) so a stuck producer fails
typed before the client's retry ladder ends. **One premise in the ruling was
re-verified and does not hold** — see §4 · **Original status:** review requested · **Reviews:**
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) §12 (the M0 results) against
§2.2 (the media-time contract) · **Written:** 2026-08-23 against `main` @
`f522eff7`, branch `agent/vod-m0` · **Asks for:** a decision on §2.2 and a
ruling on two numbers, not a re-run of the measurements

Companion to the plan's own review round
([VOD-PRESENTATION-PLAN-REVIEW.md](VOD-PRESENTATION-PLAN-REVIEW.md) and its
[response](VOD-PRESENTATION-PLAN-REVIEW-RESPONSE.md)) and to the builder's
entry point
([VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md](VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md)).
This document exists because M0 did the job M0 exists for: **P0 failed a
clause, and the failure lands inside a normative section the implementing agent
is not allowed to amend.** M1 is blocked until this is settled.

## 1. What you are being asked to decide

Everything measured is in plan §12 and the raw JSON under `target/vod-probe/`.
Nothing below asks you to trust a summary — each claim names the section that
carries its numbers.

| # | Decision | Where the evidence is |
|---|---|---|
| 1 | How a repositioned producer recognises a plan boundary, replacing §2.2's three sentences | §12.3 |
| 2 | What the plan does with a segment `CutPolicy` cannot hold under the byte ceiling | §12.4 |
| 3 | Whether D1's `playback.vod_block_secs = 8` is the right shape of answer, given it is a function of a client constant | §12.7, §12.9 |

Ledger D1, D4 and D6 are already marked resolved in §9 — those are M0's to
change and it changed them. §2.2 is not, which is why it is here.

## 2. Decision 1 — §2.2's addressing rule is unimplementable as written

### 2.1 The three sentences

From §2.2, "The media-time contract":

1. > A producer repositioned to a plan boundary seeks with `-noaccurate_seek
   > -ss <boundary>` (lands at the RAP at-or-before) and the segmenter discards
   > fragments until **the first whose DTS equals the boundary**.
2. > a mismatch (never-equal) is a typed producer failure and an index
   > invalidation, not a silent drift.
3. > **Video-only makes the index (and therefore the plan) valid for every
   > audio selection.**

### 2.2 What was measured — plan §12.3

- **Raw DTS never carries film time.** Across 43 sampled boundaries on 9
  fixtures, the emitted DTS equalled the plan boundary **3 times**, and all
  three were coincidences on one fixture whose seeked grid happens to contain
  the same tick values. `-avoid_negative_ts make_zero` rebases every
  repositioned generation to zero: seeking to 3.503 s and to 10.510 s produce
  the *same* DTS sequence. `-copyts` does not change this in any placement
  tested, with or without `-avoid_negative_ts disabled`.
- **So sentence 2 fires on every reposition.** As written, the first far seek
  of the first VOD playback invalidates the index.
- **Sentence 3 is half true.** The production pipe's video DTS grid sits a
  *constant* offset ahead of the video-only index's, and the offset is a
  function of the audio branch: 0 ticks with no encoder delay, +6 on the
  open-GOP fixtures, **+678** on `clean-cra-2397`, which is 42.4 ms at a 16000
  timescale — the AAC encoder's own delay, admitted into the video timeline
  because `make_zero` shifts by the earliest timestamp across all output
  streams. The fragment **sequence** is identical for every audio selection
  (§12.2, 9/9 fixtures: same count, verdicts, durations, output byte counts).
  The **timeline** is not.
- **`-ss` always lands early.** With the `{:.3}` string the producer formats
  (`transcode/mod.rs:1165-1174`), `-noaccurate_seek -ss` landed at least one
  fragment before the target in **43 of 43** cases — a boundary at 3.5035 s is
  not expressible in milliseconds, and on Matroska the demuxer lands at the
  enclosing cluster besides. The discard step is mandatory and its length is
  not derivable from the requested time.

### 2.3 The replacement the measurements point at

Offered as input, not as a decision taken:

> **Identify the landing fragment by its output byte count against the index,
> then count planned boundaries forward from it; the segmenter rebases `tfdt`
> to the plan's `start_ticks` as §2.2 already requires.**

Scored against the same 43 cases: byte matching resolved the landing
**uniquely 43/43**, and from that landing the repositioned generation
reproduced the t=0 fragment sequence **43/43**. It needs no timestamp, so the
audio-branch offset and the millisecond-rounding both stop mattering. The
alternative — production's existing `keyframe_probe_args` origin added to the
emitted DTS — was exact on 20/43 and within one frame on 34/43, with a worst
error of 1014 ticks (63 ms); it disagrees with the true landing on 15 of 43
because it filters packets to `≥ start`, so on a boundary that *is* a keyframe
timestamp it reports the keyframe the demuxer did not land on. **That is
exactly the input a VOD reposition always has.**

**What to attack.** D4 (§9) says entries are keyed by film-time DTS "not by
fragment ordinal (so an ffmpeg upgrade that re-fragments differently is
detected, not silently misaligned)". Byte matching keeps that detection — a
re-fragmenting upgrade changes the byte counts and the match fails loudly —
but a reviewer should check whether it degrades under two conditions the
corpus did not produce: a stream where two fragments share an output byte
count near the landing, and a landing fragment the index never held.

## 3. Decision 2 — the byte ceiling is not a ceiling, and §2.1 promises one

Plan §12.4: segments on the 143 Mb/s fixtures reach **125.2 MB** against
`COPY_SEGMENT_MAX_BYTES` of 64 MiB — 5 segments on `dense-2397`, 4 on
`dense-open-2397`. The mechanism is in the shipped policy, not in this plan:
`CutPolicy::cut_before` returns `None` until the floor is passed
(`fmp4.rs:1774-1776`), so neither ceiling can bind inside a segment's first
6 seconds, and 6 s at 143 Mb/s is already 107 MB. **Today's producer does the
same thing on the same file**, so this is not a regression the plan introduces.

It reaches the plan because §2.1 makes the playlist immutable and states
`#EXT-X-TARGETDURATION` upfront, and because the web player mirrors a 64 MB
expectation (`index.html:6849-6850`). The plan currently says nothing about a
planned segment the policy cannot bound. **Decide what it says** — the options
seen from here are to accept it as today's behaviour and document it, to make
the plan refuse to admit such a rendition (a §2.4 admission question), or to
change the policy, which §7 forbids without a decision.

## 4. Decision 3 — D1's number is a function of somebody else's constant

Plan §12.7 measured four server models against four block lengths on stock
hls.js. The result inverted the obvious reading: **the block length was never
the constraint; the deadline is.** hls.js aborts a blocked fetch at its own
`maxTimeToFirstByteMs` of 10 000 ms, so a server deadline *above* that is worse
than useless — at 15 s the client aborted five times and received **zero**
typed 503s, and the entire refusal mechanism was unreachable. At 8 s a **60 s**
block reached the end through five 503s with no fatal error and no client
configuration change.

So `playback.vod_block_secs = 8` is correct today and correct only because of
a constant inside a vendored player. Two things a reviewer should rule on:

- **Should the invariant be written down rather than the number?** Something
  like "the server deadline must be strictly below the smallest declared
  `block_budget_secs`, and a client declaring a budget at or above its own
  fetch timeout is a client bug" makes the coupling explicit and survives an
  hls.js upgrade. §2.3 currently says `min(server cap, the client's declared
  budget)`, which permits the broken configuration.
- **What bounds a producer that never materializes?** hls.js gives 7 attempts
  and then a fatal `fragLoadError` — **~31.6 s** of continuous
  `segment_pending`. §2.3 has no rule that a producer must fail typed before a
  client's retry budget runs out, and without one the client's error is
  `fragLoadError` rather than the typed refusal the plan designed.

**Re-verified after the ruling, and the correction does not hold.** The ruling
states the vendored `hls.min.js` default `maxTimeToFirstByteMs` is `8e3`, which
would make an 8 s deadline tie the client's abort timer rather than sit under
it. It is `1e4`. The `8e3` values in the bundle belong to `certLoadPolicy` and
`keyLoadPolicy`, neither of which governs a fragment fetch:

```text
certLoadPolicy:{default:{maxTimeToFirstByteMs:8e3,maxLoadTimeMs:2e4
keyLoadPolicy:{default:{maxTimeToFirstByteMs:8e3,maxLoadTimeMs:2e4
fragLoadPolicy:{default:{maxTimeToFirstByteMs:1e4,maxLoadTimeMs:12e4
```

The runs agree: every abort landed at 10 000–10 005 ms, and the
`deadline-8000` arm recorded **zero aborted attempts at all four block
lengths**, which an 8 000 ms abort timer could not produce. The invariant is
applied as ruled; it resolves to 8 s for web, with margin rather than by race.
The ruling's other half is correct — `index.html:5188` configures no load
policy, so M4 setting `fragLoadPolicy` explicitly stands.

Also worth a reviewer's eye, though not a decision: hls.js **ignores
`Retry-After`** (the header is read only for HTTP 429 in its content-steering
controller), backing off on its own 1/2/4/8/8/8-second ladder. §2.3 specifies
`Retry-After` on the typed 503. It is not wrong to send it — AVPlayer and
Media3 are unmeasured — but the plan should not assume it steers web retry
cadence.

## 5. What this brief is *not* asking for

- **Not a re-run of the measurements.** Every number is reproducible:
  `bash scripts/vod-probe-fixtures.sh` then `python3 scripts/vod-plan-probe
  target/vod-probe/fixtures/*.mkv --boundaries 5 --json out.json`, and
  `scripts/vod-probe-stub p3 --models no-deadline,per-request,deadline-8000,deadline-15000
  --delays 5,15,30,60 --configs default --json out.json`. If you disagree with
  a number, the harnesses are the argument.
- **Not a review of the harnesses' code.** They already had an adversarial
  review round; §12.8 lists what they do **not** cover, and the device halves
  of P2 and P3 are explicitly unclaimed.
- **Not a re-opening of D2, D3, D5, D7–D12.** M0 was permitted to change D1,
  D4 and D6 and changed exactly those.
