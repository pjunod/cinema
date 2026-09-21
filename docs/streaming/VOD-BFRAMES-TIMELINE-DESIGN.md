# Encoded VOD B-frames — a timeline-contract design, not a flag

**Status:** design contract complete; production remains no-reorder pending
fleet and device evidence · **Executes:** Q2 / F-stream-2 (design item,
M–L) from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
§0, §3.1, §5.3 and the assessment's correction 2 · **Written:** 2026-09-20
against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [VOD-ENCODING.md](VOD-ENCODING.md) (the no-reorder decision this
document proposes to revisit, §"Timing decisions" item 5) and
[SEGMENTER-PLAN.md](SEGMENTER-PLAN.md) §4.2 (the cut-cleanliness rule the
fragment classifier implements). Read §2 first: it is the contract as the
tree enforces it today, and every option in §3 is judged against it. Work
the design milestones in §5 in order; §5.4 records the later evidence
boundary. If a step
appears to require deleting or loosening the landing check in
[`vodgen.rs`](../../crates/plurxd/src/vodgen.rs) rather than replacing it
with a check that proves the same property another way, stop and flag it —
that is the failure mode the assessment withdrew the first-draft remedy for.

**Correction to the review:** none on the contract. One narrowing: the
appendix (F-stream-2) says `establish_or_verify` is the "existing physical
grid check". It is not — `establish_or_verify`
([`vodserve.rs:7084`](../../crates/plurxd/src/vodserve.rs)) proves *init*
identity and engine currency; the frame-grid check is
`validate_encoded_fragment` ([`vodgen.rs:378`](../../crates/plurxd/src/vodgen.rs)).
The revision-3 text already says so; this document plans against
`vodgen.rs`.

## 1. Objective and decision

Decide whether encoded VOD may emit reordered pictures (B-frames), and if so
under what timeline contract, such that every property the current
`-bf 0` rule buys is still proven by a validator rather than assumed from
encoder flags. This design phase settles the box and timeline contract and
provides an executable oracle. It does not change a Rust validator, an
encoder recipe, a setting, or production behaviour.

**Decision:** Option C remains the deployed recipe. Option A is the only
admissible reordered design if later evidence justifies a build: signed
version-1 `trun` composition offsets, the presentation-grid validator in
§3.4 landing before any reordered recipe, and qualification per encoder
family and client. Option B is rejected. No efficiency figure is credited
until it is measured on this pipeline — the appendix's 10–20 % was withdrawn
by §0 of the review as unmeasured here. Therefore the current nonzero-CTO
refusal stays deployed while the measurement, compatibility and device rows
in §5 remain blocked.

## 2. Contract today

Re-verify every citation at build time; line numbers are from `88a3957a`.

### 2.1 The encoder is told not to reorder

[`crates/plurx-core/src/transcode/vod.rs:270-299`](../../crates/plurx-core/src/transcode/vod.rs),
inside `vod_pipe_args`, appended after the family-specific encoder
arguments so it overrides them for every family:

```rust
        // No reorder delay: the plan addresses presented frame boundaries,
        // not a decoder preroll hidden before the URI's declared start.
        "-bf".to_owned(),
        "0".to_owned(),
        "-flags".to_owned(),
        "+cgop".to_owned(),
        "-g".to_owned(),
        grid.frames_per_segment.to_string(),
        "-keyint_min".to_owned(),
        grid.frames_per_segment.to_string(),
        "-sc_threshold".to_owned(),
        "0".to_owned(),
        "-fps_mode:v".to_owned(),
        // The fps filter already made CFR. Output vsync must not duplicate
        // the first video frame across the audio-only encoder preroll.
        "passthrough".to_owned(),
        // A bare -enc_time_base also quantizes AAC to video ticks. Keep this
        // video-only; audio retains its sample clock and encoder priming.
        "-enc_time_base:v".to_owned(),
        format!("{}:{}", grid.denominator, grid.numerator),
        "-avoid_negative_ts".to_owned(),
        "disabled".to_owned(),
        "-use_editlist".to_owned(),
        "0".to_owned(),
        "-movflags".to_owned(),
        // The runner removes encoder priming and restores the film-global
        // audio lattice. Per-generation edit lists must not change the init
        // or apply a second priming shift after fragment publication.
        "+empty_moov+delay_moov+default_base_moof+frag_keyframe".to_owned(),
        "-video_track_timescale".to_owned(),
        grid.numerator.to_string(),
```

Also from the same function, the IDR grid and the preroll trim
([`vod.rs:252-254`, `:176-185`](../../crates/plurx-core/src/transcode/vod.rs)):
`-force_key_frames expr:eq(mod(n,F),0)` with `F = grid.frames_per_segment`,
and a video filter prefix `trim=start=target,fps=R:start_time=target,` with
suffix `,tpad=stop_mode=clone:stop_duration=…,trim=end_frame=N,setpts=PTS-anchor/TB`.
The encoder therefore sees its first frame *at* the entry boundary; there is
no decoded preroll for it to reorder against.

The four arguments that carry the timeline contract, and what each buys:

| Argument | Buys | Why it is there |
|---|---|---|
| `-bf 0` | every sample has `pts == dts`; `cto == 0` | the plan addresses presented boundaries; decode time is presentation time |
| `-flags +cgop` | closed GOP: no picture after an IDR in decode order references anything before it | random access at every entry without leading pictures |
| `-use_editlist 0` | no `elst` in the init | an `elst` would differ per generation (`-ss`-started) and change the immutable init |
| `-avoid_negative_ts disabled` | ffmpeg does not rebase timestamps at the muxer input | `-copyts` film time reaches the muxer unchanged; the runner does the rebasing |

### 2.2 The landing check refuses any composition offset

[`crates/plurxd/src/vodgen.rs:376-406`](../../crates/plurxd/src/vodgen.rs):

```rust
    /// Encoder flags are an instruction, not proof. Refuse off-grid or
    /// non-random-access output before publishing even the first URI.
    fn validate_encoded_fragment(&mut self, fragment: &Fragment) -> Result<(), Outcome> {
        let Some(init) = &self.encoded_init else {
            return Ok(());
        };
        let Some(video) = init.video().and_then(|video| fragment.track(video.id)) else {
            return Ok(());
        };
        let Some(entry) = self.generation.plan.entry(self.encoded_entry) else {
            return Err(landing_failed(
                "encoded video exceeded its immutable plan".into(),
            ));
        };
        let origin = self
            .generation
            .plan
            .entry(self.generation.start_entry)
            .expect("start entry")
            .start_ticks;
        if !plurx_core::fmp4::classify(fragment, init).is_clean()
            || video.base_decode_time != entry.start_ticks - origin
            || video.duration() != entry.duration_ticks
            || video.samples().any(|sample| sample.cto != 0)
        {
            return Err(landing_failed(format!(
                "encoded entry {} does not match its clean frame grid: dts {}, duration {}, expected {} + {}",
                entry.index, video.base_decode_time, video.duration(), entry.start_ticks - origin, entry.duration_ticks,
            )));
        }
        self.encoded_entry += 1;
        Ok(())
    }
```

Four conjuncts, each proving one thing: (1) the fragment opens on a clean
random-access point per `fmp4::classify`; (2) its `tfdt`, relative to the
generation's first entry, equals the plan's `start_ticks`; (3) its summed
sample durations equal the plan's `duration_ticks`; (4) every sample has a
zero composition offset. (4) is the one B-frames break, and it is doing
more than one job: with `cto == 0` everywhere, (2) and (3) together prove
that the *presentation* interval of the fragment is exactly the plan's
entry, because presentation time equals decode time. Remove (4) and (2)+(3)
prove only the decode interval.

### 2.3 What the parser already understands

[`crates/plurx-core/src/fmp4.rs:190-206`](../../crates/plurx-core/src/fmp4.rs)
— the resolved sample carries a *signed* offset:

```rust
pub struct Sample {
    pub duration: u32,
    pub size: u32,
    pub size_at: Option<usize>,
    pub flags: u32,
    /// Composition offset (pts − dts). Signed here even though ffmpeg writes
    /// version-0 unsigned offsets over a shifted dts, because a version-1
    /// `trun` is legal and negative is what it means.
    pub cto: i64,
}
```

and `parse_trun` ([`fmp4.rs:3117-3123`](../../crates/plurx-core/src/fmp4.rs))
reads a version-1 `trun`'s offset as `raw as i32 as i64`. So the parser is
not the obstacle. `classify` ([`fmp4.rs:3180-3232`](../../crates/plurx-core/src/fmp4.rs))
returns `CleanIdr` for H.264 NAL 5 and HEVC NAL 19/20 *without* looking at
the sample timeline; only HEVC CRA/BLA (16–18, 21) consults
`has_leading_picture` ([`fmp4.rs:3271-3285`](../../crates/plurx-core/src/fmp4.rs)),
which computes `pts = dts + cto` per sample and reports whether any sample
presents before the first. That function is exactly the presentation-order
check the new validator needs, and it is currently unreachable for the
NAL types x264 and x265 emit at an IDR.

### 2.4 Init identity across generations

[`crates/plurxd/src/renditiondir.rs:63-67`](../../crates/plurxd/src/renditiondir.rs)
and `served_init_for` (`:133-159`): the rendition stores
`muxer_init` (SHA-256 of ffmpeg's raw `ftyp`+`moov`) and `served_init` at
first generation; every later generation's init must digest identically or
the generation ends `Failure::InitDrift` (`MuxerDrift`). The doc comment
records the measured property this rests on: the muxer init "is
byte-identical across generations including `-ss`-started ones — M0-P0
clause (d), 9/9". Any change to the SPS/PPS or `moov` that depends on where
a generation starts breaks this at the second generation.

### 2.5 Audio lattice

[`vod.rs:8-16`](../../crates/plurx-core/src/transcode/vod.rs):
`VOD_AUDIO_RATE = 48_000`, `VOD_AAC_FRAME_SAMPLES = 1_024`,
`vod_audio_anchor(start) = floor((start − 2) × 48000) / 1024 × 1024`.
`place_encoded_audio` ([`vodgen.rs:410-475`](../../crates/plurxd/src/vodgen.rs))
discards AAC frames before the film-global lattice point at or after the
entry start and refuses a generation whose audio "lost its global sample
phase". Audio is rebased independently of video; the two tracks' relative
offsets inside ffmpeg's container are not used. VOD-ENCODING.md item 4
records why: an independently regenerated entry must carry the same audio
interval as the original.

### 2.6 Recipe identity

[`crates/plurxd/src/vodencode.rs:173-200`](../../crates/plurxd/src/vodencode.rs)
`Encoding::identity` hashes, among other inputs, every argument
`self.args(file, 0.0, duration)` returns, length-delimited. Any change to
`vod_pipe_args` therefore produces a new `SourceIdentity` and, through
`rendition_key` ([`vodserve.rs:7543`](../../crates/plurxd/src/vodserve.rs)),
a new rendition directory. Encoded renditions carry the
`encoded-process` marker (`vodserve.rs:142`) and are reconciled away at the
next daemon start, so there is no durable encoded cache to migrate.

### 2.7 The decision on record

VOD-ENCODING.md, "Timing decisions and their costs", item 5:

> **No reordered video across the boundary.** Encoded VOD requests closed
> GOPs and disables B frames. This sacrifices some compression efficiency
> to make random-access and decoder-time ownership explicit. A later
> quality improvement can restore reordering only with production decode
> tests proving that leading pictures, init identity, and every planned
> boundary remain correct.

This document is that "later quality improvement" being designed. The
sentence names the three proofs; §3.6 adds two the review found
(restart splices and prepared handoff) and one the parser found (init
identity depends on SPS VUI fields reorder changes).

## 3. Change

### 3.1 What reordering changes, mechanically

Take x264 with `-bf 2` (decode order `I P B B P B B …`, frame duration `d`,
closed GOP of `F` frames). Per GOP the encoder emits `F` samples in decode
order; the decode-order index of the IDR of GOP `k` is `k·F`, exactly as
today, because a closed GOP has no picture belonging to GOP `k` decoded
before its IDR. What changes is per-sample:

```text
 decode order:   I0   P3   B1   B2   P6   B4   B5   ...
 dts (frames):    0    1    2    3    4    5    6
 pts (frames):    0    3    1    2    6    4    5
 raw cto:         0   +2   -1   -1   +2   -1   -1      (pts - dts)
```

ffmpeg's mov muxer has two ways to write this:

- **Version-0 `trun` (default).** Offsets must be unsigned, so the muxer
  shifts dts down by the reorder delay (`2d` here) and writes
  `cto ∈ {2d, 4d, d, d, …}`. The first sample's dts is `−2d`; because
  `-use_editlist 0`, nothing tells the player to skip the delay, so the
  presentation starts `2d` late and every fragment's presentation interval
  is `[k·F·d + 2d, (k+1)·F·d + 2d)`. With `-avoid_negative_ts disabled`
  the negative first dts reaches the muxer as is, and the `tfdt` written is
  `cluster[0].dts − start_dts`, still 0 for the generation's first
  fragment. Conjuncts (2) and (3) pass; (4) fails on every sample; and the
  *presentation* is off the plan by the delay.
- **Version-1 `trun` (`-movflags +negative_cts_offsets`).** The muxer
  subtracts the first sample's offset from every offset, so the first
  sample has `dts = cts = 0` and later samples carry signed offsets
  (`0, +2d, −d, −d, …`). Decode order at every IDR is `k·F·d`, the IDR's own
  offset is 0, and the presentation interval of GOP `k` is exactly
  `[k·F·d, (k+1)·F·d)`. Conjuncts (1)–(3) pass; (4) still fails, because
  the P and B samples have nonzero offsets. **This is why the flag alone
  fails the invariant:** the invariant as written is "no sample has an
  offset", and version-1 offsets are offsets. The property the invariant
  was standing in for — "this fragment presents exactly its planned
  interval" — is *satisfied* by version-1 output under closed GOP, but
  nothing in the tree checks that property directly today, so removing (4)
  would leave (2)+(3), which prove decode intervals only. The correct
  change is to replace (4) with a presentation-grid check (§3.3), not to
  delete it.

The muxer arithmetic above is from reading ffmpeg's `movenc` behaviour, not
from a run on this pipeline. §5.4 B0 makes it a parser-backed fixture (`ffprobe
-show_packets` plus this repo's `fmp4::parse`) before anything depends on
it, on both the CI ffmpeg 6 and the shipped jellyfin-ffmpeg 8.

### 3.2 The five things B-frames touch

1. **Composition offsets in the frame grid.** Covered above. Also: the
   plan's `est_bytes` and `target_duration`
   ([`vod.rs:66-102`](../../crates/plurx-core/src/transcode/vod.rs)) are
   unchanged — they are per-entry, not per-sample.
2. **Leading pictures at random access.** `+cgop` forbids them for x264.
   For x265 and `hevc_qsv` (the HDR10 grade, Q12), IDR NAL types 19/20 are
   classified clean without inspection, and an `IDR_W_RADL` *may* carry
   decodable leading pictures whose `pts` precedes the IDR's; those samples
   belong to the previous entry in presentation and to this one in decode.
   The presentation-grid check catches this regardless of NAL type; the
   NAL-type classifier does not.
3. **Init identity.** With reordering, x264 writes VUI
   `bitstream_restriction` with `num_reorder_frames` and
   `max_dec_frame_buffering` into the SPS, and the `avcC`/`hvcC` in `moov`
   changes accordingly. That is a *new recipe* (new key, §2.6), fine. The
   obligation is that these fields are a function of the recipe only, not
   of the generation's start entry or its remaining frame count (`-t`,
   `trim=end_frame`) — otherwise generation 2 fails `MuxerDrift`. Expected
   true for x264/x265 (they derive the values from `bframes`/`b-pyramid`
   settings); to be *proven* per family (§3.6).
4. **Restart splices.** A generation starting at entry `k` encodes from
   the entry's first frame (the `trim` prefix); its first fragment is the
   IDR of entry `k` with `tfdt = 0` and, under version-1 offsets, `cto = 0`
   — the same shape as entry 0 of a full run. Two generations' bytes for
   entry `k` are *not* identical today either (rate-control history
   differs); the contract is grid identity, not byte identity. A client
   that plays entry `k−1` from generation A and entry `k` from generation
   B decodes each from the shared init; closed GOP makes them independent.
   The new hazard is the encoder's *first-frame delay*: the version-1
   shift is computed from the generation's first packet, so it must be the
   same constant for every generation of a recipe. It is, if the encoder's
   reorder depth is fixed by the recipe; a family that adapts depth to
   content (hardware B-pyramid heuristics) would fail conjunct (2) on the
   first fragment, which is the right failure.
5. **AAC lattice.** Untouched in samples: reordering is video-only, audio
   keeps its 1024-sample lattice and `place_encoded_audio` is unchanged.
   Interleaving changes — with `delay_moov`+`frag_keyframe` the muxer
   still cuts on the keyframe *in decode order*, which is still the IDR —
   so the fragment boundaries are the same. The one thing to watch is that
   `-t` is applied to encoder input frames, not output, so the delayed
   last frames still flush inside the same `-t`; the final entry's
   `trim=end_frame` bound is the real limit and is unchanged.

Not touched: `-force_key_frames`, `-g`, `-keyint_min`, `-sc_threshold 0`,
`-enc_time_base:v`, `-video_track_timescale`, `-use_editlist 0`,
`-avoid_negative_ts disabled`, the `fps` sampling, the `setpts` rebase.

### 3.3 Options

**Option A — version-1 offsets with a presentation-grid validator
(the only admissible reordered candidate).** Add `+negative_cts_offsets` to the
`-movflags` string and `-bf N` per family, and replace conjunct (4) in
`validate_encoded_fragment` with three checks over the sample list:

```text
 let pts_i = base_decode_time + Σ_{j<i} duration_j + cto_i
 (a) min_i pts_i            == entry.start_ticks − origin
 (b) max_i (pts_i + dur_i)  == entry.start_ticks − origin + entry.duration_ticks
 (c) the set {pts_i} is exactly the arithmetic grid
     start, start+d, …, start+(n−1)·d   with n == sample_count
```

(a)+(b) are the presentation interval; (c) is the frame grid (no dropped
or duplicated presentation slots, which `fps` + `-fps_mode passthrough`
promise today and (3) proved only in decode order). Keep (1)–(3) as they
are. `has_leading_picture` becomes a special case of (a) and can stay for
the copy path. Cost: one pass over the samples per fragment, already
parsed. Risk: the check is new code on the publication path; it needs a
table test with synthetic `trun`s (version 0 and 1, positive and negative
offsets, a leading picture, a duplicate pts) before it guards production.

**Option B — decode-time grid (dts-shifted).** Keep version-0 offsets,
express the plan in decode time and let presentation run `delay` late
uniformly. Rejected as a default: entry 0's `tfdt` would have to be
negative (unsigned field) or an `elst` would be needed (`-use_editlist 0`
exists to forbid per-generation edit lists), and every consumer of "film
time" — subtitle windows, control positions, marker prewarm, the audio
lattice — would need the same shift applied. It moves the contract into
every caller instead of one validator.

**Option C — keep no-reorder.** Zero risk, known cost. Correct if §5.4 B0
measures a gain the fleet does not need at its bitrates, or if any family's
proofs in §3.6 fail and the gain is not worth carrying two recipes.

This document makes the architectural decision: Option A is the sole shape
that may be evaluated and Option B must not be built. It does not make the
deployment decision. Option C remains deployed until B0's measured numbers,
family proofs and consumer evidence justify a per-family change with Paul.

### 3.4 Normative fragmented-MP4 and timeline contract

The abstract fixture
[`tests/playback/vod-bframes-timeline-cases.json`](../../tests/playback/vod-bframes-timeline-cases.json)
and its executable judge
[`tests/operations/test_vod_bframes_timeline_design.py`](../../tests/operations/test_vod_bframes_timeline_design.py)
are the design oracle. They deliberately do not call production code: this
PR must be able to prove the proposed contract without making the proposal
live. A future implementation must port the same checks into
`validate_encoded_fragment` and then exercise them with parser-produced
fragments.

The candidate's box contract is:

1. `tfdt` owns decode time. Its resolved unsigned base-media-decode-time,
   whether encoded as version 0 or 1, equals the planned entry start relative
   to the generation origin. It is never shifted to hide reorder delay.
2. `trun` owns composition time. Any fragment with a nonzero composition
   offset uses version 1 and the sample-composition-time-offset-present flag;
   each on-wire offset is a signed 32-bit `pts - dts`. Today's all-zero output
   may remain version 0. No `ctts` or per-generation init state is introduced.
3. `tfhd` and `trex` defaults remain legal. The validator operates on the
   parser's resolved sample durations, regardless of which box supplied each
   value. Every resolved video-sample duration must equal
   `Encoding.grid.denominator`; the output may not define its own grid.
4. `-use_editlist 0` remains, so neither `elst` nor any other presentation
   shift may repair a fragment after the fact. `moof`/`traf` are self-contained
   against the recipe's immutable `moov`.
5. All arithmetic is checked after widening to a signed type able to represent
   `u64 + u32 + i32` (an `i128` in the proposed Rust implementation). Overflow,
   a negative PTS, or a value outside the planned interval is a typed landing
   refusal, never a wrap or clamp.

For sample `i`, with `D_i` its resolved duration and `C_i` its resolved CTO:

```text
dts_i = tfdt + Σ(j < i) D_j
pts_i = dts_i + C_i
```

The fragment is accepted only when the existing clean-random-access check
passes, `tfdt == planned_start`, every `D_i == frame_ticks`, the entry duration
is divisible by `frame_ticks`, the sample count is exactly that quotient, the
decode duration equals the entry duration, and the sorted PTS multiset is
exactly:

```text
planned_start, planned_start + frame_ticks, …, planned_end - frame_ticks
```

This single exact-multiset comparison proves interval start and end, rejects
leading pictures, and rejects duplicated or missing presentation slots. The
implementation should retain named refusal reasons for the individual
preconditions and interval diagnostics; a generic boolean would make a fleet
regression needlessly opaque. The fixture includes the current zero-CTO form,
a valid `I0 P3 B1 B2` version-1 form, unsigned shifted offsets, a leading
picture, a duplicate slot, a missing slot, a wrong `tfdt`, a nonuniform
duration, a non-clean first sample and an out-of-range signed CTO.

### 3.5 Compatibility decision

ISO BMFF legality is necessary, not sufficient. A producer or consumer row is
qualified only by the named observation; unknown is a refusal to enable that
family, not evidence inherited from another family.

| Surface | Candidate representation | Evidence required before enablement | State at this design boundary |
|---|---|---|---|
| CI ffmpeg 6, x264/x265 | version-1 `trun`, signed CTO, no edit list | parse every fragment, independently decode every entry, compare init across entry 0/mid/last starts | blocked: fixture encode not run |
| media1 jellyfin-ffmpeg 8, x264/x265/QSV families actually selected | same; fixed reorder depth per recipe | same fixture plus family inventory, restart splice and init-digest evidence | blocked: read-only fleet run not made |
| Web hls.js/MSE | consume cold mid-film entry and both directions of prepared handoff | P4/P7 device protocol, including first two seconds in presentation order | blocked: device evidence |
| Safari native HLS and tvOS | consume version-1 CTO without edit-list compensation | P4/P7 device protocol | blocked: device evidence |
| Android Media3 | consume version-1 CTO without edit-list compensation | P4/P7 device protocol | blocked: device evidence |
| Any unobserved encoder or client | none | its own producer/parser/device row | refused by default |

The eventual control, if one is justified, is advisory-only in Settings →
Developer: it may show the readiness rows and select a qualified recipe, but
readiness information never gates unrelated playback. The selected value and
family must enter recipe identity. Until a row is qualified its family remains
at `-bf 0`; there is no global reorder default and no feature gate.

### 3.6 Proof obligations (for Option A)

Each is a test or a device observation, named in §6. None is satisfied by
"the argument string is valid".

| # | Obligation | Proven by |
|---|---|---|
| P1 | Random access at every entry: each segment decodes standalone from the served init; first VCL is IDR; presentation interval and grid per §3.3 (a)–(c) | validator table test + real-ffmpeg fixture decode of every entry independently (`ffmpeg -i init+seg -f framemd5`) |
| P2 | Init identity across generations: `MuxerDrift` never fires for generations started at entries 0, 1, mid-film, last | extend `encoded_vod_ntsc_gets_decode_after_forward_and_backward_restarts` (`vodencode_tests.rs:1090`) and `encoded_vod_manual_audio_correction_keeps_restart_init_stable` (`:1614`) to the reorder recipe |
| P3 | Restart splice: entry `k−1` from generation A followed by entry `k` from generation B decodes with no dropped/duplicated presentation slot | the same restart tests, asserting decoded frame count and `framemd5` continuity across the splice |
| P4 | Prepared handoff: a viewer switched from a no-reorder rendition to a reorder rendition (and back) at a segment boundary sees no gap on web (hls.js/MSE), Apple (native HLS), Android (Media3) | device runs (§6.3 GPT prompt); the server side is unchanged because each rendition has its own init and playlist |
| P5 | Audio lattice unchanged: `place_encoded_audio` accepts every generation; A/V offset at entry boundaries measured ≤ 1 audio frame | `encoded_vod_two_hour_audio_restart_budget` (`:1102`) run against the reorder recipe |
| P6 | Per family: x264 (SDR), x265 and `hevc_qsv` (HDR10 Main10), `h264_qsv`; NVENC/VAAPI/VideoToolbox only if a fleet node uses them (review §8 open question) | fixture matrix per family on the node that runs it |
| P7 | The three clients plus Safari native HLS decode version-1 `trun` offsets correctly at a cold start on a mid-film segment (not entry 0) | device runs (§6.3) |
| P8 | Fallback is typed: a family whose first fragment fails conjunct (2) or (a)–(c) ends the generation `landing_failed` and the rendition is not published | existing `a_refusal_after_the_landing_fails_the_generation` (`vodgen.rs:988`) with a reorder fixture |

## 4. Guardrails (non-goals)

1. **Do not delete or weaken conjunct (4) without replacing it** with
   §3.3 (a)–(c) in the same commit. The assessment's correction 2 is the
   reason: "do not remove the guard merely to make new flags pass".
2. **Do not enable `+negative_cts_offsets` on the copy path or the rolling
   HLS path.** Copy carries the source's own offsets and `classify`
   already reads them; rolling is MPEG-TS/EVENT and outside this
   contract. Scope is `vod_pipe_args` only.
3. **Do not add an edit list.** `-use_editlist 0` stays; an `elst` is a
   per-generation object and would change the immutable init (§2.1).
4. **Do not credit an efficiency number that was not measured on this
   pipeline** (review §0). §5.4 B0 produces the number; its evidence quotes it
   with the corpus, encoder, and VMAF model.
5. **Do not treat NAL type as proof of no leading pictures for IDR types.**
   The presentation check is the proof; `classify` stays as the fast
   first conjunct.
6. **Do not change the recipe for an existing rendition in place.** A new
   argument set is a new `Encoding::identity`; that is the mechanism and
   it needs no help. No migration, no in-place rewrite of `identity.json`.
7. **Do not gate with an in-code feature flag.** If a switch is needed
   during qualification it is a replicated setting surfaced in Settings →
   Developer with an advisory (never blocking) readiness list, and its
   value enters the recipe identity (§3.5).
8. **Do not bundle** with Q1 (rate control), Q3 (tone-map), Q5 (audio) or
   Q7 (master playlist). Each has its own oracle (review §7.3).

How the assessment's dispositions are honoured: F-stream-2 "Reject
flag-only remedy" → §3.1 explains why the flag alone fails and §3.3
replaces the check rather than the flag; "prove restart splices" → P3;
"Closed GOP alone is not the whole timeline contract" → §2.1 table names
all four arguments and §3.2 what each still does; "efficiency figures are
not evidence from the deployed pipeline" → §5.4 B0 measures on the shipped
ffmpeg with the repo's corpus before any decision.

## 5. Milestones and build boundary

S-12 is one design PR. Its milestones are logical commits and log rows in
this PR; no milestone gets a separate PR. Product implementation, fleet
measurement and device qualification are explicitly outside this design-only
boundary.

### 5.1 D0 — verify the deployed refusal

Read the live parser, recipe and publication path. Record that
`validate_encoded_fragment` accepts only `sample.cto == 0`, that the parser
already resolves signed version-1 offsets, and that `Encoding::identity`
hashes the effective argv. Do not edit Rust.

Acceptance: the cited source still says those three things, and the workboard
and PR say that production remains no-reorder.

### 5.2 D1 — select the only admissible box and timeline design

Choose Option A as the sole reordered candidate and reject Option B. Keep
Option C deployed. Specify the resolved `tfdt`/`trun`/`tfhd`/`trex` ownership,
signed range and checked arithmetic, the immutable no-edit-list rule, exact
presentation multiset, random-access condition, family boundary, recipe
identity and advisory-only control semantics.

Acceptance: §3.4 is unambiguous enough to implement without choosing a second
timeline policy, and §3.5 treats every unknown compatibility row as refused.

### 5.3 D2 — make the design executable

Keep the data-only fixture at
`tests/playback/vod-bframes-timeline-cases.json` and the independent judge at
`tests/operations/test_vod_bframes_timeline_design.py`. They cover valid
zero-offset and signed-reordered fragments plus the fail-closed cases listed
in §3.4. This is a design oracle, not a substitute for a parser-produced Rust
test.

Acceptance: the focused Python test passes and would fail if the valid reorder
case were rejected or any declared adversarial case were accepted.

### 5.4 D3 — record the honest stop

Mark S-12 blocked on the named producer, quality and device evidence. The PR
contains no Rust, setting, feature gate, recipe or default change. Record the
following build order for a future implementation only; it must be revalidated
against then-current source and delivered under the repository's then-current
one-plan/one-PR protocol:

1. **B0 — measure and capture real boxes.** Run `scripts/bench rate-control`
   on the existing corpus plus animation and grain-heavy clips, report bitrate
   at equal VMAF and VMAF at equal bitrate per family/model, and produce
   parser-backed fragments on CI ffmpeg 6 and media1 jellyfin-ffmpeg 8. Assert
   signed version-1 offsets, exact PTS grid, independent entry decode and
   generation-stable init digests.
2. **B1 — deployment decision.** Record B0's numbers and Paul's dated A-or-C
   decision in VOD-ENCODING.md. If C wins, stop.
3. **B2 — validator first.** Port §3.4 into
   `validate_encoded_fragment` while the recipe remains `-bf 0`; add
   parser-produced table cases and named refusals. Observe fleet refusals
   before changing output.
4. **B3 — qualified recipes only.** For each family proven by P1–P3, P5, P6
   and P8, add signed-offset reorder args and an identity-bearing control. Any
   Developer surface is advisory-only and shows readiness; it never gates
   unrelated playback. Unqualified families stay at `-bf 0`.
5. **B4 — consumers and default.** Run P4/P7 on web, Safari, tvOS and Android.
   Only qualified family/client combinations may change default. Keep a
   bounded-outcome metric with labels drawn from fixed enums, then record the
   observation and rollback evidence.

Acceptance: the board names the external evidence instead of claiming it, and
the execution log distinguishes completed design from blocked build work.

## 6. Verification and rollout

### 6.1 Design lane for this PR

- `python3 -m unittest tests.operations.test_vod_bframes_timeline_design -v`
- `python3 -m unittest tests.operations.test_docs_index -v`
- `python3 -m json.tool tests/playback/vod-bframes-timeline-cases.json`
- `git diff --check`

There is no Rust edit, so this PR does not establish a compiler loop or spend
a workspace unit run. The Rust and real-ffmpeg commands named by P1–P8 belong
to B0–B4 and run only after the deployment decision authorizes a build.

### 6.2 Only a node can prove

The ffmpeg 8 fixture run (B0) and the per-family encodes (P6) run on
media1 (QSV) and, if Paul wants the NVENC/VAAPI/VideoToolbox rows, on
whichever lab node or Mac has the device. Record `ffmpeg -version` in the
PR body.

### 6.3 Only a device can prove (GPT prompt)

```text
Plurx B-frame VOD qualification. Server: media1 (10.42.0.10), build with
playback.vod_reorder_frames=2 on the test library only.

For each client — web (Chrome, hls.js), Safari (native HLS), Apple TV
(tvOS app), Android TV (Media3 app) — and for each of the two test titles
"Harbor Lights" (SDR, 23.976) and "Night Tide" (HDR10, 24):

1. Cold start at 0:00 on the encoded rung (force quality below source).
   Record time to first frame and whether the first second shows a
   frozen/duplicated frame.
2. Seek to 47:13 (a mid-film entry). Record time to moving picture and
   whether the first frames after the seek are in order (no backwards
   step, no stutter within the first two seconds).
3. Change quality once (prepared handoff, Settings → Developer shows the
   prepared successor) and once more back. Record any gap, freeze or
   audio dropout at the switch.
4. Let it play 10 minutes. Record stalled seconds from the playback-info
   panel and any "landing_failed" in Settings → Logs.

Then repeat 1–3 with playback.vod_reorder_frames=0 on the same titles and
report both tables side by side. Do not report the badge as evidence of
picture correctness; report what the screen did.
```

### 6.4 Eventual rollback, if B3 is built

The future implementation must make zero reorder the rollback value. New
sessions then get the old recipe key; running renditions keep theirs until
they end. No migration is allowed either way. This describes acceptance for
B3; the setting does not exist in this design PR.

## 7. Open questions

1. Which hardware families does the fleet actually select? Review §8 asks
   for the inventory from `/metrics`; P6's row set depends on it.
2. Whether hls.js's passthrough remuxer computes its start time from
   version-1 `trun` offsets correctly in the vendored build — a P7 item,
   to be read in the vendored source before the device run, not assumed.
3. Whether `hevc_qsv` keeps a fixed reorder depth for a fixed `-bf` on the
   media1 driver, or adapts it (which would fail conjunct (2) on the first
   fragment by design). B0's fixture on media1 answers it.
4. Whether the gain at the fleet's actual bitrates (Q1's rate-control
   work may move them) justifies carrying the reorder recipe per family.
   B1 decides with B0's table in hand.

---

## Execution log

Executing sessions append one row per logical milestone in the single plan PR
(see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Claim | [#423](http://192.168.4.7:3000/noirr/plurx/pulls/423) | Claimed design-only S-12 from `6063b37c`; verified current `validate_encoded_fragment` rejects any nonzero CTO before publication. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | D0–D3 | [#423](http://192.168.4.7:3000/noirr/plurx/pulls/423) · `868fd806` | Selected C as deployed and A as the sole future candidate; specified box ownership, exact checked presentation grid, compatibility matrix and B0–B4 boundary; added 10-case executable oracle. Green: `python3 -m unittest tests.operations.test_vod_bframes_timeline_design -v`; `python3 -m unittest tests.operations.test_docs_index -v`; `python3 -m json.tool tests/playback/vod-bframes-timeline-cases.json`; `git diff --check`. No Rust changed, so no compiler or broad unit run. Needs: ffmpeg 6/8 boxes and quality, family/init/restart evidence, Paul's deployment decision, and web/Safari/tvOS/Android observations. |
