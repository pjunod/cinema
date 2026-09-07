# VOD M0 — every issue found

**Status:** complete list as of 2026-08-23 · **From:** milestone M0 of
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) · **Evidence:** plan §12,
`target/vod-probe/*.json` · **Branch:** `agent/vod-m0`

**All of group A was ruled on 2026-08-23 and the amendments are applied to the
plan; M1 is authorized.** A1 — byte-landing identification, strengthened to a
3-fragment byte-count sequence, with typed failure on no match or a double
match. A2 — documentation, not policy: `TARGETDURATION` bounds time, not
bytes. A3 — write the invariant, not the number, plus a 30 s producer
materialization budget. B1 and B2 became stopgaps S5 and S6; B3 folded into
M1's spec. D6 stays open until the device halves return. Plan §12.9 carries
the full rulings; the sections below are the record of what was found.

One premise in the ruling was re-verified and does not hold: the vendored
`hls.min.js` fragment first-byte default is `1e4`, not `8e3` — the `8e3`
values are `certLoadPolicy` and `keyLoadPolicy`. The invariant applies as
ruled and resolves to 8 s for web, with margin rather than by race.

Four groups. **A** needed a decision and blocked M1. **B** are production defects
found in passing, none of them fixed. **C** were defects in my own measurement
harnesses — all fixed and re-run, listed so nobody re-finds them. **D** is what
M0 did not measure.

---

## A. Blocks M1 — needs a decision

### A1. §2.2's reposition rule cannot be implemented as written

**What the plan says.** Three sentences in §2.2:

1. the segmenter "discards fragments until the first whose DTS equals the
   boundary";
2. "a mismatch (never-equal) is a typed producer failure and an index
   invalidation";
3. "Video-only makes the index (and therefore the plan) valid for every audio
   selection."

**What was measured.** Over 43 sampled boundaries on 9 fixtures:

- the emitted DTS equalled the plan boundary **3 times**, and all three were
  coincidences on one fixture whose seeked grid happens to contain the same
  tick values. `-avoid_negative_ts make_zero` rebases every repositioned
  generation to zero — seeking to 3.503 s and to 10.510 s produce the *same*
  DTS sequence. `-copyts` did not change this in any placement tried.
- so sentence 2 fires on **every** reposition: the first far seek of the first
  VOD playback invalidates the index.
- sentence 3 is half true. The production pipe's video DTS grid sits a
  **constant** offset ahead of the video-only index's, and the offset depends
  on the audio branch: 0 ticks on some fixtures, +6 on the open-GOP ones, +678
  on `clean-cra-2397` (42.4 ms at a 16000 timescale). The fragment
  **sequence** is identical for every audio selection on 9/9 fixtures — count,
  verdicts, durations, output byte counts. The **timeline** is not.
- `-noaccurate_seek -ss` with the `{:.3}` string production formats landed at
  least one fragment **early in 43 of 43 cases**. A boundary at 3.5035 s is not
  expressible in milliseconds.

**Option seen from here, offered as input not as a decision.** Identify the
landing fragment by its **output byte count** against the index, count planned
boundaries forward from it, and let the segmenter rebase `tfdt` to the plan's
`start_ticks` as §2.2 already requires. Scored on the same 43 cases: unique
landing **43/43**, and the repositioned generation reproduced the t=0 fragment
sequence **43/43**. The alternative — production's existing
`keyframe_probe_args` origin plus the emitted DTS — was exact on 20/43, within
one frame on 34/43, worst error 63 ms, and disagrees with the true landing on
15/43 because it filters packets to `≥ start`, which is exactly the input a VOD
reposition always has.

**What to attack.** D4 keys entries by film-time DTS specifically so an ffmpeg
upgrade that re-fragments differently is detected rather than silently
misaligned. Byte matching keeps that property — a re-fragmenting upgrade
changes the byte counts and the match fails loudly — but two conditions the
corpus never produced are unchecked: two fragments sharing an output byte count
near the landing, and a landing fragment the index never held.

### A2. The byte ceiling does not bound a segment, and §2.1 promises it does

Segments on the 143 Mb/s fixtures reached **125.2 MB** against
`COPY_SEGMENT_MAX_BYTES` of 64 MiB — 5 segments on `dense-2397`, 4 on
`dense-open-2397`. `CutPolicy::cut_before` returns `None` until the floor is
passed, so neither ceiling can bind inside a segment's first 6 seconds, and 6 s
at 143 Mb/s is already 107 MB.

This is the shipped policy, not something the plan introduced — but the plan
makes the playlist immutable, states `#EXT-X-TARGETDURATION` upfront, and says
nothing about a planned segment the policy cannot bound. The web player mirrors
a 64 MB expectation (`index.html:6849-6850`). Decide what the plan says:
document it as today's behaviour, refuse to admit such a rendition (a §2.4
admission question), or change the policy — which §7 forbids without a
decision.

### A3. D1's number is a function of a constant inside a vendored player

hls.js aborts a blocked fetch at its own `maxTimeToFirstByteMs` of 10 000 ms. A
server deadline **above** that is worse than useless: at 15 s the client
aborted five times and received **zero** typed 503s — the whole refusal
mechanism was unreachable. At 8 s a **60-second** block reached the end through
five 503s, no fatal error, no client configuration change.

So `playback.vod_block_secs = 8` is right today, and right only because of a
number inside hls.min.js. Two things to rule on:

- **Write the invariant, not the number?** Something like "the server deadline
  must be strictly below the smallest declared `block_budget_secs`, and a
  client declaring a budget at or above its own fetch timeout is a client bug."
  §2.3 currently says `min(server cap, the client's declared budget)`, which
  permits the broken configuration.
- **What bounds a producer that never materializes?** hls.js gives 7 attempts
  then a fatal `fragLoadError` — **~31.6 s** of continuous `segment_pending`.
  §2.3 has no rule that a producer must fail typed before the client's retry
  budget runs out, so today the viewer gets `fragLoadError` instead of the
  typed refusal the plan designed.

**Also worth knowing, not a decision.** hls.js **ignores `Retry-After`** — the
header is read only for HTTP 429, in its content-steering controller. It backs
off on its own 1/2/4/8/8/8-second ladder, measured identical whether the server
sent `Retry-After: 1` or `Retry-After: 3`. §2.3 specifies `Retry-After` on the
typed 503; sending it is not wrong (AVPlayer and Media3 are unmeasured) but the
plan should not assume it steers web retry cadence.

---

## B. Production defects found in passing — none fixed

Each is small, independent of the presentation change, and would be its own PR.
These are additions to the plan's §6 stopgap list (S1–S4), not replacements.

### B1. Nominal audio bitrate is not an upper bound

ffmpeg's AAC encoder overshoots its `-b:a 320k` target by **1.2235×** on the
5.1 branch. Any code that sizes a buffer, a budget, or a headroom from the
`-b:a` value in the argument list is sized wrong. M1's headroom constant must
be measured — a margin of at least 1.25 over nominal, or the probe's observed
rate per file.

### B2. `keyframe_probe_args` is unreliable on exactly the inputs VOD gives it

`parse_keyframe_origin` reads `ffprobe -read_intervals "{start}%+#4"`, which
**filters packets to `≥ start`**. For an arbitrary seek target the answer agrees
with where the demuxer really lands (measured within 1 ms). For a target that
**is** a keyframe timestamp it reports that keyframe — while
`ffmpeg -noaccurate_seek -ss` lands a full GOP earlier, at the enclosing
Matroska cluster. Measured disagreement: a full 1.752 s on `closed-gop-2397`,
1.502 s on `h264-2997`.

Plan boundaries are keyframe timestamps by construction, so VOD would hit the
bad case every time. Whether today's `media_origin` correction is also wrong on
some real seeks was **not** established — today's seeks are arbitrary user
times, which is the case that agrees. Worth a look; not claimed as a live bug.

### B3. The audio tail changes `TARGETDURATION` and only a fixture with one shows it

`audiotail-2397` (25 s of audio past the video) plans 4 video entries plus **2
audio-tail entries covering 24.991 s**, and its honest `TARGETDURATION` is
**15**, not the **8** a video-only plan emits. An understated
`#EXT-X-TARGETDURATION` is a spec violation players act on. §2.2 already
requires the tail be computed from the probe's per-track durations — this is
the number that says it matters.

---

## C. Defects in my own harnesses — all fixed, listed so nobody re-finds them

Two adversarial review rounds, one per harness. Everything below was fixed and
the measurements re-run; the numbers in group A are post-fix.

**`scripts/vod-plan-probe` — 9 blocking, of 16 found.**

- `plan_invariants` was computed and never scored, so a destroyed `CutPolicy`
  transcription produced four green clauses over a 52-entry nonsense plan.
- `read_pipe` never checked the child's exit status or fragment coverage, so a
  pipe dying 21 % into a film reported PASS on every clause — and two
  identically truncated runs satisfied the determinism clause.
- the fixture corpus was all-clean or all-dirty per fixture, so `CutPolicy`'s
  clean-before-ceiling ordering and `Segmenter::push`'s decide-before-accumulate
  ordering were both unfalsifiable. Two fixtures added (`mixed-2397`,
  `audiotail-2397`).
- the index-to-production DTS shift was applied to plan entry 0, which never
  carries it, measuring a two-fragment segment over one.
- a non-constant offset silently degraded to "no offset" and was then reported
  as a tested hypothesis.
- the byte-bound pass criterion contained a tunable `--audio-margin` dial;
  `--audio-margin 12` turned a real failure into a pass.
- no counterpart to `Segmenter::finish()`'s audio-tail split, which made
  `TARGETDURATION` wrong on any trailing-audio title (this is what surfaced B3).
- `stderr=PIPE` could deadlock against ffmpeg on a chatty source.
- entries the byte check could not resolve were skipped silently while the
  clause still reported the full entry count.

**`scripts/vod-probe-stub` — 3 blocking, and one of them inverted an answer.**

- the block was modelled as "materialize N seconds after the first request,
  no deadline" — a server §2.3 explicitly rejects. Composed correctly the P3
  conclusion **inverted**: the first pass said stock hls.js tolerates ~50 s and
  dies at 60 s; it actually survives 60 s through typed 503s, and the binding
  constraint is the deadline, not the block. Everything in A3 comes from the
  corrected model.
- `status: "measured"` meant "the page POSTed", not "the browser decoded
  anything" — with hls.js 404ing it printed PASS and exited 0 over an empty
  request log.
- one degenerate page crashed the summariser mid-sweep and the report was only
  written after the loop, so a 16-run sweep dying on run 15 left nothing.
- the "nominal" playlist copied the segmenter's measured tail instead of the
  plan's remainder, manufacturing the −0.627 s under-declaration the first pass
  reported as a finding.
- `Retry-After: 1` aliased hls.js's own first backoff step, so the original
  evidence for "the header is ignored" was consistent with both hypotheses. Now
  proven three ways.
- the jitter fixture's `toFixed(6)` rounded half-up, so 10 of 30 boundaries
  landed a frame late and the fixture did not implement its documented pattern.

---

## D. What M0 did not measure

Named so nobody reads a green run as coverage it is not.

- **Every device half.** P2 and P3 speak for hls.js only. AVPlayer and Media3
  have their own first-byte timeouts and their own 503 handling, and D1's
  number is the **minimum across all three**. Protocols are in STATUS.html's
  VOD operator checklist.
- **P1 on real titles.** Sandbox throughput (40–549 MB of source per second on
  local disk) says nothing about nynuc over NFS with a cold cache, which is the
  number that sizes the background indexing job.
- **P0 on ≥10 representative real files.** §8 M0-P0 asks for the corpus *and*
  real files; only the corpus ran. The fixtures are adversarial, not
  representative.
- **Dolby Vision preservation.** `-tag:v dvh1`, `-strict unofficial`, and the
  Profile-5 branch that omits `-bsf:v` entirely change the copied NAL stream
  and therefore every byte count in A2 and B1. No DV source was reachable.
- **A/V-corrected sources.** With `audio_offset_ms != 0` on the copy-audio
  branch, production builds a second `-itsoffset` input; two demuxers
  interleaving into one muxer is a different fragment stream.
- **Producer pacing.** `-readrate` cannot change output bytes, but the probe's
  argv differs from the daemon's logged one.
