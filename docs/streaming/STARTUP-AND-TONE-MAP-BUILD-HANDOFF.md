# Startup and tone-map build handoff — shorter encode gate, cheaper create, measured SDR picture

**Status:** rev 6 — all of Astra's findings closed; every part's design is
approved (third review, 2026-10-04). Part A passed the §4.4 gate · **Executes:** startup items 1 and 2 and quality item 2 from Paul's
2026-10-04 request · **Written:** 2026-10-04 against main `16f002b70`
(#805); main has since moved to `70b72f3be` (#811, #806) — see §8.3 ·
**Fleet:** all four nodes ran `v0.3.0-6559-g16f002b70` when measured ·
**Build progress:** [STATUS.md](../../STATUS.md)

## Who does what with this document

| Section | Goes to | State |
|---|---|---|
| §5 Part B — create-path reads | **Opus: build** | Approved |
| §6 C-M0, C-M1 — tone-map census and tidy | **Opus: build** | Approved |
| §4 Part A — short rolling target | **Opus: build**, starting with A-M0 | Design approved. A-M0's lab receipt must pick the bootstrap before A-M1 starts; QSV waits for A-M2b |
| §6.3 C-M2, C-M3 — tone-map alignment and selection | **Paul: ruling R2** first | Design approved; blocked on R2 |

Nothing in this document is waiting on Astra.

Companion to the quality-and-streaming follow-ups build handoff of the same
date (a project document, not in this repository), which carries everything
else from the same request.

Read §0 for what the adversarial review changed, §1 for what is being built
and why, §2 for the rulings Paul owes, then build §4, §5 and §6 — each is
independent of the others and has its own contract, guardrails and
milestones. Work milestone by milestone. Every anchor was read at
`16f002b70`; **re-verify each against current main before editing, because
five other PRs are open on neighbouring files (§8.3).** If a step seems to
require anything §3 lists as a non-goal, or a contract here disagrees with
the code you find, stop and bring it to Paul instead of choosing.

Host names follow the public-mirror rule (PR #332): media1 = the remote
Intel node, lab3 and lab4 = the Intel lab nodes, lab6 = the AMD node,
`forge.lan:3000` = Forgejo. Use those names in anything you commit.

## 0. What the adversarial review changed

Rev 1 was reviewed by two independent agents against the code. They found
ten blockers. Rev 2 is narrower and says less than rev 1 did, on purpose.

| Rev 1 said | Review found | Rev 2 |
|---|---|---|
| Copy sessions take a 6 s target when Matroska Cues bound the keyframe gap | The strict cut already caps a segment at the target; what a short target costs on a copy is dirty cuts, one dropped frame each, and the tree measured and reverted exactly that (`plurx-core/src/transcode/mod.rs:119-131, 177-187`). Cues cannot tell a clean keyframe from a dirty one. | **Copy stays at 16 s.** Non-goal 2 says what would unlock it. |
| Every 16 s-derived constant scales with the new target | The governing plan forbids it (`PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md` §4.2, lines 214-218), and at 4 s the hard deadline falls under the producer progress budget, the flow guard is clipped and slow playback retires the session | Only three roles move: the advertised target, the publication cadence and first-snapshot readiness. Steady reserve, allowance, flow guard and scratch sizing keep today's constants. |
| Encodes take 4 s | 6 s unchanged-playlist tolerance is below `PRODUCER_PROGRESS_BUDGET` (10 s); 0.25× playback retires 2 s segments | **8 s** is the build value. 6 and 4 are lab arms only. |
| Every encode is eligible | A QSV or NVENC encoder may lose its forced-IDR flag (`encoder.rs:1310-1330`); legacy-lease and Jellyfin sessions are unqualified transports | Eligibility requires an encoder that cuts on our keyframes and a first-party client with an explicit lease |
| The worker's second planning-snapshot read is a duplicate | It **is** the dispatch revalidation (`manager/create.rs:496-524`) | One worker read stays; savings restated |
| Probe-JSON reads are duplicates | They run the HEVC census, which measures and writes | Reusable only once the census is recorded |
| Read-backs fold into the writing transaction | `txn` returns row counts only; the read-backs detect idempotent replay | One combined projection read after each write |
| ≤ 12 reads, ≤ 6 writes, p50 ≤ 900 ms | Not derivable; seven writes are on the path | Bounds come from B-M1's measurement |
| Census on a second relabelled-SDR fixture | `VIDEO-QUALITY-PROGRAM.md:225-226`: "Merely relabeling SDR as PQ is not a valid HDR fixture" | Census uses the authored PQ signals `scripts/tone-map-calibration` already builds |
| Align GPU graphs to the CPU's hable operator | The recorded plan (Q8c) goes the other way, OpenCL to bt.2390, and warns against substituting an operator across filters blind | The operator is Paul's ruling, made with stills in front of him (R2) |
| Census runs after readiness on its own | Unattributed GPU work while viewers play | Operator-triggered, through admission, visible and stoppable |

A second pass on rev 2 found three more blockers, all in Part A's
eligibility, and rev 3 carries the fixes:

| Rev 2 said | Second pass found | Rev 3 |
|---|---|---|
| Decide "explicit-lease client" at create | Every session is built `RollingLeaseMode::Legacy` (`playback_control.rs:9551`) and becomes Explicit only on its first accepted control exchange (`:10275`) | Eligibility is a candidate at construction; the policy is frozen at the first-snapshot commit, where the lease mode is known and nothing has been served |
| An encode can decline takeover at `start.rs:1646-1650` | That refusal is in the copy start; a successor re-decides from its own switch and could serve 16 to a client holding 8 | A candidate session writes `typeless_playlist: false` into its recipe, which the takeover path already refuses |
| "Encoder proved forced IDR" | Read literally it excludes VA-API, software and VideoToolbox, which need no flag | Predicate restated |
| 24 s first snapshot | The tree records buffer deficits at 24 s and none at 32 s at 1.05× production, on different geometry | Confronted in §4.3; the lab gains a 1.05× arm and 32 s is the recorded fallback |
| Census on the calibration script's signals | They are neutral grey panels, 8 frames — no chroma, no gamut | The census authors its own chromatic PQ signal |

Astra's review of rev 3 (verdict: request changes) raised seven findings
against this document. Each was checked against `16f002b70` and each
holds. Rev 4:

| Finding | What was wrong in rev 3 | Rev 4 |
|---|---|---|
| R01 (P1, A) | The policy was frozen at the first-snapshot commit, but readiness, endpoint selection and the response bytes are all computed before that commit (`rolling/session.rs:958-964` against `:1333`). Followed literally, the first response waits 48 s and says 16, then the session freezes 8. | A provisional policy is computed at the top of every pre-snapshot cycle and used for validation, readiness, selection and serialisation; the commit freezes that same value (§4.1) |
| R02 (P1, A) | A 16 s cadence for non-consuming viewers contradicts an advertised 8 s target: RFC 8216 §6.2.1 requires the next segment-bearing playlist within 1.5 × target | The freshness deadline always follows the advertised target; demand state changes only how fast the reserve grows (§4.1) |
| R03 (P1, A) | Eligibility was an encoder-family check. It did not cover the output actually produced (HDR10 `hevc_qsv` is probed with `force_idr=false`, `ffmpeg.rs:3354-3381`), audio tails, EOF or 4× | Eligibility is a list of proven output tuples, and each is qualified by running the real segmenter (§4.2, A-M2) |
| R04 (P1, B) | "The start still checks size and mtime" is false in general: the source fence is captured only for optional retention starts (`manager/start.rs:1689-1721`) and may fail without stopping playback (`rolling_provenance.rs:1-2`) | The rolling start keeps its own fresh read (§5.2 step 3) |
| R06 (P2, C) | `declares_filter_option` proves an option is listed, not that the graph runs on this GPU | The changed graph is executed at boot; refusal falls back to the incumbent string (§6.3) |
| R07 (P2, C) | The identity paragraph was wrong, and so was the earlier adversarial correction it came from: encoded-VOD identity hashes every argv element (`vodencode.rs:779-812`) | The three identity paths are stated separately; `CODEC-AND-GPU-QUALIFICATION.md:570-572` is correct and is not edited (§6.3) |
| R08 (P2, B) | B-M4 was to be measured by toggling Part A's switch on one build, which cannot show Part B's effect | A baseline-build arm and a candidate-build arm, each recording its own build id (§9.1) |

Astra's second review, of rev 4, approved Part B's design, cleared
C-M0 and C-M1, and left four findings. Each was checked and each holds.
Rev 5:

| Finding | What was wrong in rev 4 | Rev 5 |
|---|---|---|
| R12 (P1, A) | "Compute a provisional policy while `clock.served` is `None`" is also true after a producer retry: `clear_attempt` and `reset_for_attempt` keep the policy but clear `served` (`rolling/publication.rs:224-230, 272-282`; called at `rolling/session.rs:1670-1675`). A presentation that had served 16 could recompute and serve 8 | The provisional value is computed only while the presentation's policy-bound flag is false; the flag and the policy are set together with the first served snapshot (§4.1) |
| R13 (P1, A) | NVENC was on the eligible list with no segmentation proof; the boot probe only shows the build accepts the flag (`encoder.rs:1080-1135`). The same was true of QSV until its receipt exists | The list is what has been proven. This build ships software only; QSV joins in its own commit with its receipt; NVENC is out (§4.2) |
| R14 (P2, C) | The executed-probe fix lets two nodes on one release run different strings for the same renderer, and a version bump cannot tell them apart in the shared cache | The effective, probe-selected graph configuration is fed into the plan identity and is what the argv is built from (§6.3) |
| R15 (P2, A) | The 32 s fallback had no test that could pass it: the lab always opened at 3 × target, and A-M2 hard-coded 24 s, 30 s and 96 s | Target and bootstrap are separate lab parameters; one constant is selected by the receipt and every production expectation is derived from it (§4.3, A-M0, A-M2) |

## 1. Objective — three changes, three measured problems

| Part | Problem, as measured | Change | What "done" looks like |
|---|---|---|---|
| A (§4) | Every rolling session waits for 48 s of media before its first playlist (3 × a fixed 16 s target) — including encoded sessions, which cut exact 2 s segments and need no such allowance. | Encoded rolling sessions on a proven output, from qualified clients, advertise an 8 s target and publish their first snapshot at 24 s (or 32 s if the lab says 24 s is too little). Everything else is untouched. | On a rolling transcode, media-at-first-snapshot is the selected bootstrap instead of 48 s, with no added stall, skip or false capacity rejection across the device matrix. |
| B (§5) | The create handler on media1 took 2,520 ms with 33 store reads and 6 writes; the same request took 59 ms on a single-node Mac. All 33 are quorum reads, awaited one after another. | Remove true duplicate reads, batch adjacent settings reads, halve the registration read-backs. No consistency class changes, no read moves across a fence. | A test pins the counts; B-M1's measured counts fall by every removal proven in §5.2; the handler's store time on media1 falls in proportion. |
| C (§6) | S-07 corrected the CPU tone-map only. Three of four nodes tone-map on `vpp_qsv`, a driver curve, admitted by a gate that compares one synthetic frame by mean Y/U/V within 24 levels. Nobody knows whether the pictures differ visibly. | Measure every graph on every node against the CPU chain on real PQ signals, put numbers and stills in front of Paul, then align and select per his ruling. | Each node reports per-graph distance from the CPU chain; Paul has ruled on the house operator; the graphs that can be aligned are. |

Part C's population is HEVC PQ → SDR transcodes that take the node's GPU
graph (`heavy_source`, `plurx-core/src/transcode/mod.rs:1460-1464`;
`handles`, `transcode/pipeline.rs:226-249`). HLG, Dolby Vision Profile 5
and HDR passthrough never use it.

### 1.1 The evidence each part stands on

**Part A.** `ROLLING_PRESENTATION_TARGET_SECS = 16`
(`crates/plurx-core/src/transcode/mod.rs:196`) feeds
`ROLLING_INITIAL_RUNWAY_MS = 3 × 16 s`
(`crates/plurxd/src/transcode/rolling/publication.rs:111-112`).
`first_ready_end_ms` refuses the first served snapshot until that much
media exists (`publication.rs:253-265`, `rolling/session.rs:960-966`), and
`served_live_playlist` rewrites every rolling playlist's target to the
constant (`rolling/segment_index.rs:576-580`) — including encoded sessions
that cut 2 s closed-GOP segments (`plurx-core/src/transcode/mod.rs:82`,
`:1582`, `:1994-1998`; `transcode/tests/chunk_03.rs:2518-2531` pins the
2 → 16 rewrite). A client's own start rule keys on the advertised target —
Safari started only once complete segments past the opening reached
2 × TARGETDURATION in all three probe runs on 2026-10-02 — so shortening
the server's wait without shortening the advertised target does not help
native players. The governing plan says the same
(`docs/streaming/PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md` §4.4) and
requires "a concrete follow-on design" before a different fixed target is
built. **§4 is that design; Astra's review of this document is the
re-review §4.4 requires.**

**Part B.** `docs/streaming/PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md:386-389`
records the two numbers. The log line they came from labels its counters
backwards: `http/hls/create.rs:2303-2304` prints
`authority_reads = after[0]` and `local_reads = after[1]`, but index 0 is
`LocalRead` and index 1 is `AuthorityRead`
(`plurx-core/src/store/hiqlite.rs:661-675`). So the "33 local reads" are
33 quorum reads through the leader. That is plausibly most of the 2.5 s —
another document measured about 55 ms per read on a different path
(`docs/streaming/TCL-CANDIDATE-CATALOG-RCA-AND-FIX.md:573`) — but it is
not established: the measurements doc itself declines the cross-machine
comparison (`:392-395`), and the handler also runs the HEVC census (up to
6 s) and a source inspection on NAS. B-M1 measures the split before
anything is promised.

**Part C.** Boot logs read on 2026-10-04 from the running fleet:

| Node | Selected graph | Speed-up over CPU | Note |
|---|---|---|---|
| media1 | `vpp_qsv` | 4.55× | first candidate; later candidates never tried |
| lab4 | `vpp_qsv` | 3.30× | same |
| lab3 | `vpp_qsv` | 4.41× | same |
| lab6 | `libplacebo_vaapi` | 2.19× | `tonemap_vaapi` rejected: "VAAPI driver doesn't support HDR" |

`docs/streaming/TONE-MAP-CHAIN-CORRECTIONS.md` §4 put GPU graphs out of
S-07's scope in as many words and deferred operator changes to
`docs/streaming/CODEC-AND-GPU-QUALIFICATION.md` (Q8c / M4, unexecuted).

## 2. Rulings Paul owes

| # | Question | Recommendation | Blocks |
|---|---|---|---|
| R1 | Advertised target for eligible encodes. | **8 s.** It is the smallest value that keeps every existing invariant: 1.5 × 8 = 12 s unchanged-playlist tolerance stays above the 10 s producer progress budget, and 2 s segments survive 0.25× playback. It halves the gate. 6 and 4 need those invariants changed first. **Cost you should know:** a paused short-target session reaches the point where the server stops extending its playlist in about six minutes instead of ten, so a long pause ends in a reopen sooner (§4.1). | Nothing — one constant; A-M0 measures all three |
| R2 | The house tone-map operator, and what a node does when its only fast graph is a driver curve. Today CPU and OpenCL are hable; libplacebo and Profile 5 are bt.2390; QSV and VA-API are whatever the driver does; the recorded plan (Q8c) would move OpenCL to bt.2390. | Decide after C-M0 puts per-graph numbers and same-frame stills in front of you. Until then nothing about selection or any filter string changes. | C-M2, C-M3 |
| R3 | A session that was a short-target candidate whose owner node dies: take over on another node, or have the client reopen? | Reopen, for the first build. It needs no new durable field and no mixed-version hazard. It applies to every eligible encode while the switch is on, including one that ended up on 16 s. Takeover can follow once the switch is default-on. | A-M2's takeover step |
| R4 | Turning the short target on by default. | After the device matrix in §4.6 passes. It ships off. | Default flip only |

## 3. Non-goals — do not do these

1. **No LL-HLS, no parts, no blocking playlist reload.** The governing plan
   keeps it out (§11.3).
2. **No short target for copy sessions.** A copy cuts only at source
   keyframes and every cut at a non-clean one drops a frame; the search
   window for a clean point is what `COPY_SEGMENT_MAX_SECS = 15` buys
   (`plurx-core/src/transcode/mod.rs:177-187`: "It spent a day at 6 s… a
   6 s ceiling was pure cost"). What would unlock it is a whole-file,
   replicated fact about clean-point spacing from the same fragmenting
   pipe (`IndexRow.class`, `plurx-core/src/segplan.rs:519-545`) — and a
   title that has that fact has a fragment index and normally plays as VOD
   anyway. Copy startup belongs to the governing plan's readiness sweep.
3. **No mid-session target change.** A served target must never grow under
   a client (`plurx-core/src/fmp4.rs:5730-5731`).
4. **No change to steady-state timing.** The steady reserve
   (`rolling_explicit_publication_ends`, `publication.rs:372-390`), the
   allowance, the flow guard (`rolling/flow.rs:473-494`), the reserve
   maximum and scratch sizing keep their 16 s-derived values for every
   session. The governing plan: "Do not replace every use of
   `rolling_initial_runway_ms` mechanically."
5. **No behaviour change with the switch off or for an ineligible
   session.** Same argv, same playlists, same publication instants.
6. **No settings cache, no stale reads, no local reads on the create
   path, and no read moved across the admission claim or a fence.**
   `docs/cluster/BOUNDED-REPLICA-READS-ROLLOUT.md` §4 keeps "settings that
   gate work", admission, lease and pre-mutation reads on Authority.
7. **Do not remove the worker's dispatch revalidation**
   (`validate_candidate_planning_binding`,
   `transcode/manager/create.rs:496-524`).
   `TCL-CANDIDATE-CATALOG-RCA-AND-FIX.md:533-535`: "Revalidate
   source/settings revision … at dispatch; a catalog never authorizes
   stale execution."
8. **No wire or envelope change for Part B.** `candidate_context` is
   process-private (`transcode/session_request.rs:479-492`); a test
   asserts the snapshot is absent on the wire
   (`manager/candidates.rs:1518-1520`).
9. **Do not widen `MAX_CHANNEL_DELTA`** (`pipeprobe.rs:52`), change the
   existing gate's reference, or change which graph any node selects
   before R2.
10. **Do not touch the CPU chain string** (`zscale_tone_map_filter`,
    `plurx-core/src/transcode/mod.rs:1314-1322`).
    `scripts/tone-map-calibration:46` binds to it by regex.
11. **No tone-map option ships without a boot probe on the family that
    gets it** (`CODEC-AND-GPU-QUALIFICATION.md:575-580`); a family whose
    probe refuses keeps today's argv exactly.
12. **No Live TV changes**, and no route or quality change to buy latency.
13. **No feature gate in code.** A switch chooses behaviour; readiness
    rows are advisory and never block Save (`AGENTS.md`).

## 4. Part A — an 8 s target for encoded rolling sessions

### 4.1 Contract — three roles move, everything else stays

The 16 s constant plays four roles today. Only the first three change, and
only for an eligible session.

| Role | Today | Eligible session | Sites (re-verify) |
|---|---|---|---|
| **Advertised target** — the tag the client sees, and the bound every published EXTINF must fit | 16 | 8 | `rolling/segment_index.rs:473` (`validate_rolling_target`; callers `rolling/session.rs:901` and `manager/publication.rs:1335`), `:576-580` (`served_live_playlist`); `rolling/session.rs:2836` (`playlist_target_ms` status) |
| **Publication cadence** — how often a new snapshot is due and how long one may stay unchanged, while the viewer is consuming | target 16 s, earliest 8 s, hard 24 s; batch 16 s × rate | target 8 s, earliest 4 s, hard 12 s; batch 8 s × rate | `rolling/publication.rs:103-110`; `rolling/flow.rs:269-272`, consumed in the free function `evaluate_flow` (`flow.rs:348-349`) — `FlowInputs` needs a policy field; uses at `rolling/session.rs:944, 974, 1017, 1154, 1407-1408` |
| **First-snapshot readiness** — media that must exist before the first playlist is served | 48 s × rate, clamped to [48 s, reserve max] | B × rate, clamped to [B, reserve max], where B is 24 s or 32 s per A-M0 | `RollingStartupPolicy::bootstrap_ms`, `publication.rs:138-165`; consumed at `publication.rs:253-265` and `session.rs:960-966, 988-991` |
| **Steady state** — reserve, allowance, flow guard, reserve maximum, scratch sizing, prefix removal, release allowance | 48 s × rate, +16 s, 10 s guard, 124 s | **unchanged** | `publication.rs:111-122, 338-348, 372-390`; `flow.rs:137-143, 274-277, 473-494`; `session.rs:1058-1100, 1135, 1202, 1255`; `manager/start.rs:857, 1815`; `transcode/session_request.rs:745-746` (`EXACT_RELEASE_ALLOWANCE`) |

The clamp matters: without a floor, 0.25× playback would give a 6 s first
snapshot, under the three-target rule this design rests on. The existing
test variant clamps the same way (`publication.rs:160-162`), and the
governing plan warns that the 48 s clamp "is not automatically inherited
by a new bootstrap".

**The freshness deadline follows the advertised target in every demand
state.** An unfinished playlist that says `TARGETDURATION:8` must carry at
least one new segment no later than 12 s after the previous version
([RFC 8216 §6.2.1](https://www.rfc-editor.org/rfc/rfc8216#section-6.2.1):
no earlier than half the target, no later than 1.5 × target), whether the
viewer is Starting, Waiting, Rendering, paused or seeking. Demand state
may change **how much** is published, never **how late**:

| Demand state | Next publication due | What is published |
|---|---|---|
| Active and Rendering | 8 s after the last (4 s if the early arm fires) | the batch, 8 s × rate, up to the steady reserve |
| Anything else (Starting, Waiting, paused, seeking) | 10 s after the last | at least one new segment — the existing one-per-cycle floor (`session.rs:1097-1101`) |
| Hard deadline, every state | 12 s | — |

Ten seconds, not eight, for a viewer who is not consuming: it stays inside
the 12 s deadline and slows the reserve's walk to its ceiling. It cannot
be slowed further without breaking the deadline, and that has a cost Paul
should know (R1): a paused session publishes 2 s every 10 s instead of 2 s
every 16 s, so it reaches the reserve ceiling in roughly six minutes
instead of ten, and the pause-grace deferral and
`rolling_window_budget_exhausted` retirement that follow
(`session.rs:1111-1175`) start that much sooner. Those numbers are
estimates from the constants; A-M2 measures them.

The one interval longer than 12 s that remains is that existing deferral:
once a non-consuming viewer's reserve is at its ceiling, the clock stops
publishing for up to `ROLLING_PAUSE_GRACE`. It exists today at a 16 s
target, it applies only when about two minutes of media are already
published ahead of the viewer, and this build does not change it. It never
applies to a Starting or Waiting viewer below the ceiling, which is the
case the AVPlayer −12888 failure is about.

Why 8, literal by literal:

- **Hard deadline 12 s.** AVPlayer raises −12888 when a playlist is
  unchanged for more than 1.5 × target
  (`docs/streaming/MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md:164-165`).
  `PRODUCER_PROGRESS_BUDGET` is 10 s (`playback_control.rs:84`); at 8 the
  publication clock stays the looser of the two, as it is today. (That
  constant's comment calls it "action-passive for legacy actors"; confirm
  it constrains explicit rolling sessions, and if it does not, the 12 s
  figure stands on the AVPlayer rule alone.)
- **Slow playback.** Each cycle publishes at least one 2 s segment while
  the viewer consumes `rate × T` of media. At T = 8 and the protocol floor
  of 0.25× (`playback_control.rs:265`) those are equal — 2 s published,
  2 s consumed — so the reserve does not walk to its ceiling and the
  deferral at `session.rs:1147-1160` never has to hold a segment back. It
  is an exact boundary; A-M2 asserts the reserve does not grow there. At
  T = 4 the viewer consumes 1 s per cycle and the session is eventually
  retired.
- **Early arm.** Early publication arms when the client's runway is at
  most `ROLLING_PUBLICATION_GUARD_MS` = 10 s (`session.rs:918-927`). With a
  24 s first snapshot and an unchanged steady reserve it is not armed
  permanently.
- **Readiness: 24 s, or 32 s — one constant, chosen by A-M0.** Three
  targets (24 s) is the HLS HOLD-BACK floor and covers Safari's observed
  2 × target start rule with one target to spare. The tree has a recorded
  reason to doubt 24 s (§4.3), so the bootstrap is a separate value from
  the advertised target: `ENCODED_SHORT_BOOTSTRAP_MS`, written **B**
  below, is 24 000 or 32 000 according to A-M0's receipt, and every
  number in this document that depends on it is derived from it.

**The seam already exists.** `RollingStartupPolicy`
(`publication.rs:138-165`) is "Frozen at create", has a test-only
small-bootstrap variant (`WebFixedHlsV1`, 32 s), and the session code is
already written for it: "Keep steady production/allowance intact. Only
first snapshot readiness and endpoint selection use the frozen policy"
(`session.rs:957-961`), plus the low-reserve catch-up that publishes every
completed object until the steady reserve is reached
(`session.rs:1066-1090`, `uses_small_bootstrap`). Add one production
variant:

```rust
pub(super) enum RollingStartupPolicy {
    #[default] Conservative,
    /// Candidate encode whose explicit demand was accepted before the first snapshot.
    EncodedShortTarget,          // advertised 8 s, cadence 8/4/12 s, bootstrap B × rate
    #[cfg(test)] WebFixedHlsV1,
}
```

`uses_small_bootstrap()` is true for it. Give the policy three more
methods — `advertised_target_secs()`, `publication_target()` /
`_earliest()` / `_hard()`, `batch_ms(rate)` — and route the sites in the
first two table rows through the session's policy instead of the
constants. `Conservative` returns today's values from every method.

**Where it is bound — in two steps.** A session cannot know at
construction whether its client holds an explicit lease: every actor is
built `RollingLeaseMode::Legacy` (`playback_control.rs:9551`) and becomes
Explicit on its first accepted control exchange (`:10275`).

1. *Candidate, at construction.* Make "short-target candidate" a parameter
   of `start_with_audio_offset` and a field set when the `Session` is
   built (`manager/start.rs:1245-1256`). It is true when §4.2's first
   three tests pass. The copy start (`:1948-1959`) and the two
   constructors reached from inside the encode start for retained and
   cached output (`manager/rolling_retained.rs:123`,
   `manager/cache.rs:943`) always pass false.
2. *Provisional, every cycle until the presentation's policy is bound.*
   The cycle that may publish does its work in this order today: validate
   the writer playlist (`rolling/session.rs:901`), compute the budget and
   readiness under the clock lock (`:956-966`), select the endpoint and
   build the served bytes, and only then commit (`:1305-1340`). So a
   policy that is decided at the commit is decided after the response it
   was supposed to shape. Instead, at the top of each cycle:

   ```
   if clock.startup_policy_bound   → use clock.startup_policy, unchanged
   else provisional = candidate && lease is Explicit with an accepted
                      demand sequence ? EncodedShortTarget : Conservative
   ```

   **The condition is the bound flag, not `clock.served.is_none()`.**
   `served` is attempt-local: `clear_attempt`
   (`publication.rs:224-230`) and `reset_for_attempt` (`:272-282`) both
   clear it on a producer retry (`session.rs:1670-1675`) while keeping
   `startup_policy` and `startup_policy_bound`. A presentation that
   published 16 before its client's first control exchange, then accepted
   an explicit demand, then retried its producer, would have an empty
   `served` and would recompute 8. The flag already exists
   (`publication.rs:179-181, 206-214`); today it is set by
   `bind_startup_transport`, which this build stops using for that
   purpose.

   The value — bound or provisional — is taken from the same lease
   snapshot the cycle already reads (`:916-927`) and passed to **all
   four**: `validate_rolling_target`, `first_ready_end_ms` /
   `bootstrap_ms`, endpoint selection, and `served_live_playlist`. Nothing
   in the cycle may read the constants directly.
3. *Bound, at the first successful commit of the presentation, to the
   value the bytes were built with.* When `observe_publication` accepts
   and the clock's guards pass (`:1305-1340`), and only if the flag is
   still false, store the cycle's value **and** set
   `startup_policy_bound = true` in the same critical section that sets
   `clock.served`. Bound `Conservative` and never-bound `Conservative` are
   therefore different states, and only the second can become 8. Every
   later cycle of every later attempt uses the bound value, whatever its
   own `served` holds. A bound short-target presentation still may not
   start a successor attempt on an unproven tuple (§4.2).
4. *If the demand or the attempt changes before the commit completes.*
   The existing guards already abandon the cycle: a changed producer
   attempt returns at `:1300-1304` and again in the re-check under the
   clock lock (`:1334-1340`), a changed demand sequence fails
   `clock.budget_anchor_sequence != budget.demand_sequence` in that same
   re-check, and a refused observation returns at `:1324-1326`. An
   abandoned cycle has served nothing, stored nothing and left the flag
   false; the next cycle recomputes from scratch.

   One consequence worth stating: a session whose explicit demand arrives
   after the bootstrap's worth of media already exists is not penalised,
   provided nothing has been published yet. Its earlier cycles computed
   `Conservative` and did not publish (48 s not reached); the first cycle
   after the demand is accepted computes `EncodedShortTarget`, finds the
   bootstrap ready, and publishes. Once 48 s has been reached and 16 has
   been served, the presentation is bound to 16 for good. A legacy client never gets an accepted
   demand and is held to 48 s and target 16 by the same code.

Do not use `bind_startup_transport`
(`transcode/manager/create.rs:818-825`): it runs after start and binds
from a transport label, which proves nothing (`publication.rs:206-214`).

**Segments are byte-identical either way.** An encode cuts the same 2 s
segments with the switch on or off; only the served tag and the timing
differ. The encode's retained-output keys embed argv and `plan_digest`,
not the target (`manager/start.rs:622-628, 876-881`), so retained or
cached output may be reused across the switch.

**The switch.** `playback.rolling_short_target`, `0|1`, default off, read
at create alongside the other rolling settings. Developer card "Short
rolling target (encodes)" with advisory rows — lab receipt, device matrix,
whether this node's encoder is on the proven list — and `devGraduation`
text naming §4.6.

### 4.2 Eligibility

```
 at construction (candidate):
   switch on? ──────────────────────────────── no ──▶ not a candidate
       │ yes
   rolling encode, first-party create? ─────── no ──▶ not a candidate
       │ yes
   output tuple on the proven list? ────────── no ──▶ not a candidate
       │ yes
       ▼
   candidate

 at the first-snapshot commit (frozen):
   candidate AND explicit demand accepted? ─── no ──▶ Conservative
       │ yes
       ▼
   EncodedShortTarget
```

- **Proven output tuple.** Eligibility is stated on what the session
  actually produces — output codec, output grade and encoder — not on the
  encoder family. The list for this build:

  | Output | Encoder | Why it is on the list |
  |---|---|---|
  | H.264, SDR | software (libx264) | keyframes forced by expression, needs no flag; segmentation proven in CI by A-M2's real-ffmpeg test |
  | H.264, SDR | QSV **with** `caps.forced_idr` | **not in this build's list until its receipt exists** — see below |

  **A tuple is on the list only after the production segmenter has been
  run for it.** The boot inventory is not that proof: `validation_args`
  encodes half a second of synthetic video to the null muxer
  (`plurx-core/src/transcode/encoder.rs:1080-1126`) and its `forced_idr`
  verdict means only that the build accepted the flag (`:1129-1135`). It
  cuts no rolling segment, crosses no 2 s boundary and sees no audio tail.
  So:

  - *libx264* is proven by A-M2's test in CI and ships in the list.
  - *QSV* is added by its own commit (A-M2b), which carries the receipt
    of the same test suite run with `h264_qsv` on fleet hardware (prompt
    §9.4) under `docs/evidence/`. Until that commit lands, a QSV session
    is not a candidate. The fleet is three QSV nodes, so until then the
    switch changes almost nothing there — say so on the card.
  - *NVENC* is not on the list. No node has one, so there is nowhere to
    run its proof. It joins the same way QSV does if that changes.

  This is a list of what has been shown, compiled into the build. It is
  not a readiness gate: the Developer card's rows stay advisory and never
  block the switch (`AGENTS.md`).

  Everything else is **not** a candidate: any HDR10 or HEVC output (the
  `hevc_qsv` passthrough probe runs with `force_idr=false`,
  `plurxd/src/ffmpeg.rs:3354-3381`, so nothing proves its cut points),
  H.264 on VA-API, VideoToolbox or NVENC (nothing has run their rolling
  segmentation against an 8 s bound), and — once QSV is listed — a QSV
  encoder whose probe lost the flag ("segments will follow the
  encoder's own GOP rather than the requested length"). Adding a tuple
  later means adding its segmentation proof first. `self.caps` and the
  resolved grade are both in hand in the start path
  (`manager/plan.rs:1272`; `manager/start.rs:513-547`).
- **Every attempt, not only the first.** A session's producer can be
  respawned (`clear_compatibility_before_retry`,
  `rolling/session.rs:1669-1679`). Before the first snapshot, a new
  attempt whose tuple is off the list makes the session not a candidate.
  After the policy is frozen at 8, an attempt whose tuple is off the list
  must not start: the session ends with its existing typed failure and the
  client reopens. The target cannot grow, so there is no third option.
- **Every segment, including the tail.** The 8 s bound applies to every
  published EXTINF: the last video segment, an audio-only tail at end of
  stream, and a source shorter than the bootstrap (which publishes
  whole, with `ENDLIST`, as today). `validate_rolling_target` retiring a
  session after it sees an oversized segment is the backstop, not the
  proof — the proof is A-M2's segmentation test, run before the tuple is
  allowed.
- **First party.** Jellyfin compatibility creates
  (`create_for_compatibility`, `http/hls/create.rs:2240`) are never
  candidates. The governing plan: "unknown or legacy clients retain the
  currently qualified policy" (§4.2, lines 207-209).
- **Owner loss (R3).** A candidate session must not be taken over: a
  successor goes through `create_cluster_takeover_session_under_guard` →
  `start_with_audio_offset(takeover)` (`manager/create.rs:290-321`) and
  re-decides from its own switch and encoder, so it could serve 16 to a
  client holding 8. `classify_owner_loss`
  (`media_sessions.rs:3396-3421`) reads only the durable recipe, and the
  recipe already has a flag the takeover path refuses:
  `typeless_playlist: false` (`media_sessions.rs:3409, 5886`; "Local
  rolling serving never reads it", `manager/describe.rs:76-81`). For a
  candidate, write `typeless_playlist: false` at the existing post-start
  recipe rewrite (`http/hls/create.rs:3469-3470`); that needs the start
  result (`StartInfo`) to carry the candidate bit. The client then
  reopens. The candidate bit is used rather than the frozen policy because
  the rewrite happens before the first snapshot.
- **Why no durable field.** `SessionRequest` and `RemoteStartRequest` are
  `#[serde(deny_unknown_fields)]` (`session_request.rs:613-615`,
  `media_sessions.rs:1230`); the recipe is parsed at 14 sites including
  every control exchange (`http/hls/control.rs:1577, 2313`) and every
  remote placement (`http/internal_media_sessions.rs:169`), and
  `media_sessions.rs:3761-3764` classifies an unparseable row as VOD. If
  Paul rules for takeover instead (R3), the field, its write point, every
  reader and a rollback rule (switch off and drain before any downgrade)
  become part of this build.
- **Remote placement.** If the owner is another node, the policy must be
  decided by the owner from its own encoder verdict and returned with the
  start result. If that needs an envelope field, the session is
  `Conservative` on a remote owner in this build — say so on the card.

### 4.3 What must not regress

- **The recorded 32 s minimum.**
  `docs/streaming/PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md:125-128,
  235-246` records client buffer deficits at a 24 s bootstrap when
  production runs at 1.05× (14 of 64, then 44 of 180 replays) and none at
  32 s — "Keep a 32-second candidate minimum" — and `:300-302` keeps the
  candidate in test builds for "insufficient source/rate qualification".
  That replay used 8 s and 16 s objects on a 16 s cadence; this design
  uses 2 s objects on an 8 s cadence with the catch-up publishing every
  completed object, so the result may not transfer — but it has not been
  shown not to. A-M0 therefore runs target 8 with **both** bootstraps
  as separate arms, each at 1.05×. The decision rule is in A-M0; 32 s is
  used only if its own arm passes, never by default because 24 s failed.
  The existing
  `WebFixedHlsV1` test
  (`transcode/tests/chunk_03.rs:748-830`) does not settle it: no flow
  controller, no client, and it asserts only that the served end stays
  ahead of the position.
- **False capacity rejection.** With no snapshot served, the capacity
  verdict fires once `end_ms ≥ bootstrap` (`session.rs:985-991`). At 24 s
  it judges a `recent_speed` that still includes encoder warm-up. Count
  `insufficient` outcomes switch-on against switch-off in A-M3; if they
  rise, keep that one verdict at the conservative runway and say so.
- **AVPlayer's stall heuristic.** That RCA's §2.4 records AVPlayer in
  `WaitingToMinimizeStalls` with 19.9 s buffered and 32 s published,
  ending in −12888. A 24 s first snapshot is below that observation; the
  low-reserve catch-up should carry the playlist past it within one cycle.
  This is the first thing the Apple TV run looks at.
- **Apple client thresholds.** `runwayCeilingSeconds = 10` and
  `pendingMediaThresholdMs = 10_000`
  (`clients/apple/Sources/PlayerController.swift:1559-1573`) assume a deep
  ahead window. Steady state is unchanged, so they should hold; confirm on
  the device before claiming no client change is needed.
- **Position continuity.** No forward skip under 2× production — the class
  shipped once already.

### 4.4 Tests that pin today's numbers

Parameterise, do not delete; every existing case keeps passing for
`Conservative`: `transcode/tests/chunk_03.rs` (49 sites, e.g. `:406,
:703, :1287-1291, :2518-2573, :3055, :3146, :3478-3673`);
`chunk_04.rs:39, 118, 381`; `transcode/test_support.rs:54-121`;
`http/system.rs:7042, 7207, 7247`; `media_sessions.rs:7493-7507`;
`clients/apple/Tests/AppleClientTests.swift:6454`;
`tests/playback/native-hls-startup.test.js:12`;
`tests/playback/native-hls-browser-fixture.py:61`;
`scripts/playback-startup-lab:77`. Copy-path pins (`copyseg.rs:1292-1293,
1938, 2386-2391`; `plurx-core/src/fmp4.rs:7978-8013, 9722-9725`) must not
change at all. `tests/playback/rolling-producer-owners.toml` counts are
exact equality — recount on the merged tree.

### 4.5 Milestones

**A-M0 — lab arms, no product change.** Extend
`tests/playback/native-hls-browser-fixture.py` and
`scripts/playback-startup-lab` to serve a sliding playlist of 2 s
segments with **two independent parameters**: the advertised target and
the bootstrap (media present when the first playlist is released). After
release the playlist grows at 1.05×, 1× and 2×; 1.05× is the ratio that
failed at 24 s in the recorded replay (§4.3). The fixture decides when the
first playlist exists, so "starts earlier" is true by construction and is
not the criterion.

| Arm | Target | Bootstrap | Purpose |
|---|---|---|---|
| control | 16 | 48 | today |
| a | 8 | 24 | the proposal |
| b | 8 | 32 | the recorded minimum |
| c, d | 6, 4 | 3 × target | lab only; shows where it breaks |

*Acceptance:* per arm and per player (Safari native, hls.js), 30-minute
runs at each growth rate with reloads every target: stalls, buffer
deficits, position jumps, `waiting` events and time-to-first-frame after
the playlist appears, as JSON receipts under `docs/evidence/`. Then one
decision, written into the receipt:

```
 arm a clean at every rate? ── yes ──▶ B = 24 000
        │ no
 arm b clean at every rate? ── yes ──▶ B = 32 000
        │ no
        ▼
 Part A is not built. Stop and report.
```

"Arm a failed, so use 32" is not a result. If arm a fails, arm b has to
have been run and has to have passed in the same receipt. A-M1 does not
start until the receipt names B.

**A-M1 — route the three roles through the policy, behaviour unchanged.**
*Acceptance:* `make unit` green with no expectation changed;
`tests/playback/decoder-selection-m0-args.json` unchanged; a test that
`Conservative` returns the six current constants.

**A-M2 — the variant, the switch, eligibility.**
*Acceptance — each of these must be able to fail:*

Every expectation below is written in terms of B, the bootstrap A-M0
selected, and the tests read the constant rather than a literal. A
reviewer must be able to change B from 24 000 to 32 000 and see the suite
still pass with nothing else edited; a test that pins 24, 30 or 96 as a
number fails that check.

- **First response bytes.** For a candidate whose explicit demand is
  accepted before B of media exists, read the actual first served
  playlist and the next one: both say `TARGETDURATION:8`, and the first
  appears at B of media from origin (a resume adds the origin lead,
  `publication.rs:380-383` — assert at origin). A mutant that builds the
  first response under `Conservative` and stores 8 afterwards must fail
  this test; checking the stored policy alone does not count.
- **Late demand.** Demand accepted when B + 6 s of media already exists
  and nothing has been published: the next cycle publishes at once with
  target 8. Demand never accepted: first
  response at 48 s with target 16, and the stored policy is
  `Conservative`.
- **Bound survives a retry (R12).** (a) Publish 16 for a candidate with
  no explicit demand; accept an explicit demand; clear and reset the
  producer attempt; publish again: both responses say 16. (b) Bind 8;
  retry onto another tuple that is on the list; every response before and
  after says 8, and readiness for the new attempt uses the short
  bootstrap. A mutant whose only provisional test is `served.is_none()`
  must fail (a). The off-list retry case below does not cover either.
- **Change during admission.** Change the demand sequence, and separately
  the producer attempt, between the cycle's computation and its commit:
  nothing is served, nothing is stored, and the following cycle's response
  is self-consistent (its tag, its readiness and its stored policy agree).
- **Freshness.** Record every segment-bearing publication interval through
  Starting → Waiting → Rendering → paused → Rendering. For a target-8
  session none exceeds 12 s, except the existing reserve-ceiling deferral
  of a non-consuming viewer, which the test asserts separately and by
  name. A mutant that schedules a non-consuming viewer at 16 s must fail.
- **Pause cost, measured.** Time from pause to the reserve ceiling and to
  retirement, short-target against conservative, recorded in the PR.
- **Ineligible sessions serve 16 at 48 s:** switch off; a copy; an HDR10 or
  HEVC output; H.264 on VA-API or NVENC; a Jellyfin create.
- **Real segmentation, per allowed tuple.** Run the production rolling
  argv for libx264 through the real segmenter on fixtures at 23.976, 25,
  29.97 and 59.94 fps, one with audio that runs 3 s past the video and one
  shorter than B: every EXTINF, including the audio-only tail and the
  final segment, is at most 8 s, and the short source publishes whole with
  `ENDLIST`. An oversized audio tail or final segment must fail the
  test. The suite takes the encoder as a parameter so the identical cases
  run for QSV.
- **The list is what was proven (R13).** A test enumerates the compiled
  list and asserts it equals the tuples that have a committed receipt:
  libx264 at A-M2; libx264 and QSV after A-M2b. NVENC with a boot-accepted
  flag and no receipt is not a candidate. A QSV session before A-M2b
  serves 16 at 48 s.
- **Rates.** 0.25×, 0.5×, 1×, 2× and 4×, ten minutes of simulated playback
  each: every segment is published before the hard deadline; at 0.25× the
  reserve does not grow; at 4× the first snapshot waits for
  `min(B × 4, reserve maximum)` of media — 96 s when B is 24 s, the 124 s
  clamp when B is 32 s — and a producer that cannot hold 4× is rejected
  by the existing capacity verdict, not served and stalled.
- **Later attempt off the list.** After the freeze, a respawn onto an
  ineligible tuple ends the session with the typed failure; it does not
  serve a segment over 8 s and does not change the tag.
- **Owner loss.** A candidate's recipe carries `typeless_playlist: false`
  and owner loss classifies as a reopen.

**A-M2b — QSV joins the list.** Its own commit, after prompt §9.4 has
been run on a build of the A-M2 branch.
*Acceptance:* the receipt committed under `docs/evidence/` shows the full
suite — four frame rates, the long audio tail, the short source — passing
with `h264_qsv`, with every EXTINF listed; the list test is updated in the
same commit; a QSV encoder whose probe lost the forced-IDR flag is still
not a candidate.

**A-M3 — measurement.** GPT prompt §9.5.
*Acceptance:* media-at-first-snapshot and click-to-frame, switch off
against on, matched and alternated, from the
`phase = "writer_inventory_to_first_snapshot"` log line
(`session.rs:1337-1352`); zero added stalls; `insufficient` count not
higher.

### 4.6 Device matrix before default-on (R4)

From the governing plan's §8, the rows this change can break: Safari
native, physical Apple TV, physical Android TV, Chrome/hls.js × rolling
transcode, PGS burn, text burn × start at zero, resume, near end of file,
a source shorter than the runway, pause 60 s then play, pause 10 minutes
then play, seek storm, rates 0.25× / 0.5× / 2× / 4× × a 30-minute
continuity run per transport. Fail on any stall, forward skip, −12888 or
capacity rejection that the matched switch-off run does not show.

**Approved.** Astra's third review (2026-10-04) found that §4 satisfies
the governing plan's §4.4. That approves the design, not the tests,
hardware runs or default-on rollout: A-M0's receipt still selects the
bootstrap before A-M1 starts.

## 5. Part B — create path round trips

### 5.1 Contract — the inventory at `16f002b70`

Every call below is `query_consistent_map` (a quorum read through the
leader) or a write, awaited sequentially. The measured window starts
inside `create_with_purpose` (`http/hls/create.rs:2287-2300`); auth adds
one read outside it (`http/extract.rs:751`). `describe.rs` means
`transcode/manager/describe.rs`.

| Phase | Site | Call | Class | Note |
|---|---|---|---|---|
| Lookup | `create.rs:2476` | `playback_planning_snapshot`: file row, probe JSON, generation, 10 settings (`manager/candidates.rs:1174-1185`) | R | the request's snapshot |
| Settings | `create.rs:2579` → `describe.rs:894` | `get_setting(HWACCEL)` | R | value is in the snapshot |
| Settings | `network.rs:164` | `get_setting(PLAYBACK_NETWORK_PRIORS)` | R | |
| Plan | `create.rs:1387`, `:1475` | `get_setting(HWACCEL)` (Auto only) | R | value is in the snapshot |
| Plan | `create.rs:1965` → `hevc_census.rs:60-119` | census: read, and on the first play of an HEVC revision measure, **write**, re-read | R (+W+R) | not a duplicate — see §5.2 step 3 |
| Admission | `create.rs:2680` → `hiqlite_sessions.rs:1467` | `maintain_media_sessions` probe | R (+W) | |
| Admission | `hiqlite_sessions.rs:1468` | `request_row` pre-check | R | |
| Admission | `hiqlite_sessions.rs:1484` | claim insert | W | |
| Settings | `create.rs:3013` → `describe.rs:84-86` | typeless-sliding, then takeover-enabled | R, R | deliberately after the claim |
| Predecessor | `create.rs:2964`, `:3044` | route, pointer, route | R ×2–3 | deliberately after the claim (`:3038-3048`) |
| Placement | `create.rs:3151` → `media_pool.rs:1100` | media-pool switch; candidate-catalogue reads (`candidates.rs:146, 1009`; `media_pool.rs:1712`) | R ×1–3 | offers may run on a peer |
| Worker | `manager/create.rs:512` | `playback_planning_snapshot` | R | **the dispatch revalidation — stays** |
| Worker | `manager/create.rs:1928` | 6 VOD settings in one read | R | |
| Worker | `manager/create.rs:1635` | `get_file` | R | derivable from the revalidation read |
| Index | `vod/serve/create.rs:506, 526, 584`; `construct.rs:453, 479` | census again, cluster-cache switch, index source, artifact, unverified-HEVC switch | R ×3–5 | |
| Index miss | `state.rs:2875, 2894` | enqueue analysis (+ read-back), join viewer | W+R, W | |
| Fallback | `manager/create.rs:745` | live-recovery switch | R | read at the fallback decision |
| Rolling copy | `start.rs:1527, 1538, 1570, 1575` | `get_file`, census, 3 language settings, `get_item` | R ×4 | |
| Rolling copy | `start.rs:1652` → `describe.rs:102-107` | read rate, then burst | R, R | adjacent |
| Rolling copy | `start.rs:1691`; `rolling_retained.rs:21` | `CACHE_MAX_GB` (twice at start 0) | R ×1–2 | |
| Registration | `create.rs:3409` | assign owner | W | |
| Registration | `create.rs:4089` → `hiqlite_sessions.rs:1619, 1990-2027` | activate: pre-read, multi-statement `txn`, two read-backs | R, W, R, R | read-backs detect idempotent replay (`:2001-2015`) |
| Registration | `create.rs:4136` → `:3003-3066` | settle: `execute_idempotent`, two read-backs | W, R, R | the pointer read observes other writers |
| Registration | `create.rs:3795` → `:3217, 3266, 3284` | publish: write, two read-backs | W, R, R | |

Static count for the measured shape (HEVC copy, index miss, rolling
fallback, local owner): 31–38 reads and 7 writes — claim, assign,
activate, settle, publish, enqueue, join — plus the census write on a true
first play.

### 5.2 The reductions, in build order

1. **Fix the labels** at `create.rs:2303-2304`, correct the wording at
   `PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md:386-389`, and fix the stale
   "from the read pool" comment at `create.rs:2473-2475`. First, so every
   later measurement reads correctly. This is a corrective commit touching
   `crates/` — it needs its `validation/regressions.d` row.
2. **Stop re-reading what the request's snapshot already holds, on the
   request side** (saves 1–3): the `HWACCEL` reads at `create.rs:2579,
   1387, 1475`. Same task, same snapshot, nothing in between.
3. **One worker read instead of two for the VOD attempt** (saves 1–2,
   local owner only):
   `validate_candidate_planning_binding` already re-reads the snapshot at
   dispatch. Have it return the fresh snapshot and derive the worker's
   `get_file` (`manager/create.rs:1635`) and `HWACCEL` for the **VOD
   attempt** from that read, not from the request's — it is used
   immediately, in the same task, with nothing in between.
   **The rolling start keeps its own reads** (`start.rs:1527, 1538,
   1570`). A rolling fallback begins after a whole VOD attempt, and
   nothing mandatory re-checks the source in between: the source fence at
   `manager/start.rs:1689-1721` is captured only for a start at zero that
   is eligible for retention, and its contract is "Failure removes
   measured-output eligibility, never ordinary playback"
   (`rolling_provenance.rs:1-2, 37-39`). The `get_file` at `start.rs:1527`
   is therefore the only thing that makes a rolling start see a file that
   was replaced or rescanned during the VOD attempt. Removing it needs a
   mandatory execution-time fence on every rolling start with a defined
   failure; that is a design of its own and is not in this build.
   `candidate_context` exists only on candidate-route creates;
   a legacy-route create has no carrier — leave it alone rather than add
   one. **Probe JSON** may be taken from a carried snapshot only when
   `needs_census` is false for the carried value; otherwise the census
   call stays, because it measures and writes
   (`hevc_census.rs:60-119`) and its write changes the probe JSON that
   `PlanningBinding` hashes (`media_pool.rs:319-328`).
4. **Batch adjacent settings reads in place** (saves 2–3): read rate and
   burst (`describe.rs:102-107`) become one `get_settings`; the two
   `CACHE_MAX_GB` reads (`start.rs:1691`, `rolling_retained.rs:21`)
   become one, at the position of the first — they are not adjacent to
   the pacing read at `start.rs:1652`, so do not merge across. Typeless
   and takeover (`describe.rs:84-88`) are an `||` short-circuit; batching
   them saves at most one read and must never add one.
   Use `get_settings` (precedent `manager/create.rs:1926-1936`), not a new
   `Store` method and not `QUALITY_PLANNING_KEYS` (pinned at ten,
   `candidates.rs:1265`). **Do not move any read earlier.** The generation
   trigger covers only `playback.*` and `transcode.*`
   (`plurx-core/src/store/mod.rs:1731-1739`), so a media-pool, takeover,
   HLS or cache key read before its current position could flip
   undetected mid-create.
5. **One projection read after each registration write** (saves 3–5):
   activate, settle and publish each follow their write with two reads —
   route, then pointer or request row. Replace each pair with one
   statement that returns the combined projection. The reads stay after
   the write and stay authority reads; they still answer "replay or
   failure" and still observe other writers. Do not try to return rows
   from `txn` (it returns counts only,
   `plurx-core/src/store/hiqlite.rs:1589-1592`) or from
   `execute_idempotent` (a replay changes 0 rows and returns nothing).
6. **Predecessor in one read** (saves 1–2): pointer-and-route `JOIN`,
   reused for `previous_session_id`, at its current position after the
   claim.
7. **Item title** (saves 1): sub-select it into the snapshot instead of
   `get_item` at `start.rs:1575`.

Left alone on purpose: the write count; `maintain_media_sessions` on the
path; the `request_row` pre-check; any concurrency between reads that
today run in a deliberate order.

`plurx-core/src/store/consistent_read_census.rs` counts source sites, not
runtime calls, and `route_by` / `request_row` are shared helpers, so the
pinned `("hiqlite_sessions.rs", 27)` may not move. A new call site needs
an `// authority:` comment under `UNANNOTATED_CEILING` (`:82`). A changed
`Store` signature must be reflected in
`plurx-core/tests/store_contract.rs:440`.

### 5.3 The regression test

`crates/plurxd/tests/cluster_activity.rs` spawns the daemon binary
(`:93`) and talks to it over HTTP (`:753`); there is no in-process
`AppState` to wrap, and SQLite records no operation counts
(`plurx-core/src/store/mod.rs:5712-5713`). So the test is black-box:
create a session against a hiqlite-backed daemon and parse the corrected
`create startup read budget finished` line. The fixture has two nodes,
posts `copy:true` and retries on 503 (`cluster_activity.rs:90-110`), so
pin the owner node and parse the last line, or the counts vary. Pin the
shapes that are deterministic — first play with a VOD index, reopen, and
a rolling encode (which needs a new fixture file). The
index-miss shape races the background index build; pin it only if the
test can hold that build off with an existing knob, otherwise record it
from the fleet log in B-M4 and say so.

### 5.4 Milestones

**B-M1 — labels, the test, and the split.** The test asserts today's
counts for each pinned shape. Add per-phase elapsed time and summed store
time to the corrected log line.
*Acceptance:* test green; the PR records, per shape, reads, writes,
handler ms and store ms.

**B-M2 — reductions 2 to 4.** *Acceptance:* each pinned bound drops by
the calls actually removed, listed one by one in the PR; the
`PlanningBinding` golden is unchanged; the stale-execution test for
`validate_candidate_planning_binding` still fails a changed source. Add
the interval that test does not cover: replace or rescan the source
**after** dispatch validation passes and **before** the rolling fallback
starts, and assert the rolling start uses the new row's size, mtime and
probe facts. A build that starts from the pre-replacement facts must
fail.

**B-M3 — reductions 5 to 7.** *Acceptance:* bounds lowered again; the
idempotent-replay and precondition-failure tests for activate, settle and
publish still pass unmodified. Expected landing for the index-miss shape:
14–19 reads, 7 writes. If a removal turns out to be unsafe, drop it and
record why — do not relax a guardrail to hit a number.

**B-M4 — fleet measurement.** GPT prompt §9.1, which is two arms on two
builds and is separate from Part A's switch comparison (§9.5).
*Acceptance:* on media1, 20 creates per request shape on the **baseline
build** (current main with only B-M1's corrected log line and split, so
the two arms log the same fields) and 20 on the **candidate build**, same
titles, same clients, same cache state, Part A's switch off in both. Each
row records the build id from `plurx_build_info` and the reads, writes,
handler ms and store ms. A receipt whose two arms carry the same build id
is rejected — it measures nothing about Part B. The claim to check is that
store time falls in proportion to calls removed. No absolute latency is
promised until B-M1 shows what share of the handler is store time.

That makes B-M1 its own deployable commit: land it, let it be deployed and
measured as the baseline, then land B-M2 and B-M3.

## 6. Part C — the SDR picture, measured before it is changed

### 6.1 Contract — what each graph passes today

The CPU chain (`plurx-core/src/transcode/mod.rs:1316-1319`):

```
zscale=tin={tin}:min=bt2020nc:pin=bt2020:t=linear:p=bt709:npl=100,
format=gbrpf32le,tonemap=tonemap=hable:desat=0:peak={nits/100},
zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p
```

Peak is MaxCLL, else mastering maximum, else 1000 (`mod.rs:1107-1119`).
It is the baseline for comparison, not an analytical reference:
`VIDEO-QUALITY-PROGRAM.md:234-235` (H2) says to "distinguish a known
analytical reference from a historical implementation used as baseline".
Everything below measures **distance from the CPU chain**; it does not
call that distance fidelity.

| Graph | Builder | Passed today | Controls it has |
|---|---|---|---|
| `vpp_qsv` | `transcode/pipeline.rs:371-377` | `tonemap=1:format=nv12` | none — driver curve |
| `tonemap_vaapi` | `pipeline.rs:380-386` | bt709 tags, nv12 | none — driver curve |
| `libplacebo_vaapi`, `libplacebo` | `pipeline.rs:390-402` | `tonemapping=bt.2390`, bt709 terms | operator, range, dither, gamut mode (inferred) |
| CPU-pipeline libplacebo (`PLURX_TONEMAP=libplacebo`) | `mod.rs:1231-1235` | `tonemapping=bt.2390`, yuv420p | same; never probed |
| `tonemap_opencl` | `pipeline.rs:406-420` | `tonemap=hable`, bt709 terms | peak, desat, range (inferred) |
| `dovi_tonemapx` (Profile 5) | `pipeline.rs:421-428`; copies at `mod.rs:1236-1240` and `ffmpeg.rs:3107` | `tonemap=bt2390`, `range=tv`, `apply_dovi=1` | operator, peak, desat (inferred) |

No GPU graph receives `tone_map_peak`; only the Zscale arm uses it
(`mod.rs:1253`).

Selection: `pipeprobe::probe` runs once at boot (`main.rs:2701`), tries
`CANDIDATES` in order — VppQsv, TonemapVaapi, LibplaceboVaapi, Libplacebo,
TonemapOpencl, Cpu (`pipeline.rs:112-119`) — and the first pass wins
(`pipeprobe.rs:449-452`). `pairs_with` (`pipeline.rs:183-214`) makes
libplacebo and OpenCL eligible on a QSV node, so an Intel node could run
them; none has, because `vpp_qsv` passes first. The image installs VA
drivers only (`Dockerfile:128-134`) — whether libplacebo or OpenCL can
start on the Intel nodes is unknown until the census runs.

The gate (`pipeprobe.rs:489-505, 554-576`): exit status, bt709 tags, mean
Y/U/V within 24.0 of the reference (`:52`), speed-up ≥ 1.2 (`:43`). Its
fixture is `testsrc2` converted to 10-bit and tagged PQ with MaxCLL 1000
(`pipeprobe.rs:810-823`) — an SDR pattern relabelled, which is fine for
"does this graph run and land on bt709" and useless for judging a
tone-map. **The gate, its fixture and its reference do not change in this
build** (non-goal 9).

### 6.2 The census — a separate, operator-run measurement

- **Signal.** Authored by the census itself, in Rust, as real PQ code
  values — not `testsrc2` relabelled, and not the calibration script's
  cases: those are neutral grey panels, 512 × 128, 8 frames
  (`scripts/tone-map-calibration:18-19`), and the script's own report
  says "Neutral signals do not qualify gamut, skin tones, real titles,
  GPUs or physical displays". The census frame carries a grey ramp to
  each mastering peak plus patches of the BT.2020 primaries and
  secondaries and two skin-range patches at three luminance levels, in
  two versions (MaxCLL 1000 and 4000), encoded once to HEVC Main10 with
  matching metadata at a rate a hardware decoder accepts (not lossless).
  The runtime image has no Python, which is the other reason the script
  cannot be the generator.
- **Speed** comes from the existing 4K boot fixture, not from the census
  signal: eight frames time nothing.
- **Baseline.** The production CPU chain at each signal's own peak, in
  production order (scale, then tone-map — `mod.rs:1224, 1253`).
- **Candidates.** Every graph `pairs_with` allows for the node's encoder,
  not only the selected one.
- **Measures, per candidate and signal.** Whether it started at all;
  block means on a 16 × 9 grid per channel against the
  baseline, reported as maximum and 95th-percentile delta; brightest-block
  luma. One saved still per candidate from the same frame, for R2.
- **Noise floor.** The CPU chain against itself across two runs. If that
  is zero — likely, it is deterministic — say so; the eventual bar is then
  a margin Paul chooses from the stills, not a derived number.

**It is GPU work on a production node, so it is attributable and
stoppable** (Paul's standing rule): it runs only when an admin asks
(Settings → System, the "HDR tone-map" row,
`web/pages/settings-system.js:74-103, 211`), it takes a background
hardware slot through admission and yields to any foreground session — a
live start waits on a background holder until it yields
(`crates/plurxd/src/admission.rs:289, 939`), so the census polls
`background_must_yield` (`:781`) at least every 500 ms and kills its
child when it is set — the row shows
queued / running / finished with a stop control, and each ffmpeg it
spawns goes through the existing spawn seam and is added to the pipeprobe
ledgers in `tests/playback/rolling-producer-owners.toml:4230, 4737`.
Results are stored node-locally keyed by (ffmpeg version, encoder family,
GPU device) and served with `sys.tone_map`.

### 6.3 What happens after the ruling

R2 picks the operator and the treatment of driver-curve graphs. Whatever
it is, these rules hold:

- **Every changed graph is executed at boot before it is used.** A listed
  option is not a working graph: `declares_filter_option`
  (`plurxd/src/ffmpeg.rs:2969`) only finds the token in the filter's help
  text, and says nothing about whether this GPU and driver accept the
  value. The Dolby path already does both — declaration check, then a real
  run (`ffmpeg.rs:3061-3090`). So for each candidate whose string changes:
  check the option is declared; then have the boot probe run the **new**
  string through the existing gate (`pipeprobe.rs:554-576`); if either
  step refuses, run the **incumbent** string for that candidate and, if it
  passes, select the candidate with the incumbent string. The report and
  the System row say which string a node is running and why. When the
  binary does not list it, that graph keeps today's string. The pinned
  binary is jellyfin-ffmpeg 8.1.3 (`Dockerfile:57`) but it is not the only
  one in use (the Mac lab ran 9.0.1).
- **The same operator name is a different curve in a different filter**
  (`CODEC-AND-GPU-QUALIFICATION.md:563-568`). An operator change on any
  graph is accepted on its census numbers and stills, never on the name.
- **Identity — three paths, three answers.** Rev 3 had this wrong.
  1. *Encoded VOD.* `Encoding::identity`
     (`plurxd/src/vodencode.rs:779-812`) hashes the plan digest **and
     every element of `self.args`**. A changed filter string is a changed
     argv, so encoded-VOD identity already moves; old and new renditions
     cannot alias. `CODEC-AND-GPU-QUALIFICATION.md:570-572` says so and is
     correct — do not edit it.
  2. *The shared semantic cache.* `Recipe::hash`
     (`plurx-core/src/transcode/recipe.rs:98-120`) is built from the
     pipeline digest (ffmpeg build, muxer, segment policy — `:54-64`),
     the plan namespace and `plan_digest`, which feeds the tone-map kind,
     peak nits, peak source and renderer name
     (`plurx-core/src/transcode/decode.rs:2774-2788`). No raw argv, no
     driver, no graph variant — and a matching recipe is reused across
     nodes (`plurxd/src/transcode/manager/cache.rs:7-35`).
     The executed probe above means one release can run **two** strings
     for the same renderer: the new graph on a node that accepts it, the
     incumbent on a node that refuses it, or either on one node before and
     after a driver change. Those two share every hashed field. A bump of
     `RESOLVED_TRANSCODE_PLAN_VERSION` separates old releases from new
     and does nothing about this.
     So the plan must carry the **effective, probe-selected graph
     configuration** — a small typed value naming the variant actually
     chosen (incumbent or aligned) and every picture-affecting option it
     passes (operator, desaturation, range, dither, gamut mode, whether
     the peak is threaded) — and that value is (i) fed into `plan_digest`
     and (ii) the only thing the production argv for that graph is built
     from. One source, so the key and the picture cannot disagree. A
     version bump may accompany it; it does not replace it.
  3. *Rolling retained output.* Its logical record embeds the full argv
     (`plurxd/src/transcode/manager/start.rs:622-628, 876-881`), so it
     moves by itself.
- **This supersedes Q8c / M4** in `CODEC-AND-GPU-QUALIFICATION.md`. Edit
  that document in the same PR so two plans do not point opposite ways.
- **Peak.** Threading `tone_map_peak` into a GPU graph adds no identity
  field (peak is already in the digest for every pipeline), but the peak
  telemetry and the playback-info row are gated to CPU + Zscale today
  (`manager/start.rs:919-924, 1277-1278`; `web/player/stats.js:73-78`) —
  widen them with it.
- **Selection.** If R2 changes it, the choice is a setting that waits in
  Developer until qualified (`AGENTS.md:42-47`) and then graduates to
  Settings → Playback. The pipeline is a field set at construction
  (`manager/construct.rs:29`), so a change takes effect at the node's next
  restart — the card must say so. It does nothing for `dovi_tonemapx` or
  the passthrough pipelines, which bypass node selection
  (`manager/plan.rs:1237-1256`). `tone_map_pipelines` in the media-pool
  offer (`manager/construct.rs:612-636`) must follow the selection, the
  first-proof-wins test (`pipeprobe.rs:1698`) changes, and
  `docs/OPERATIONS.md:3133` ("probed at boot, not configured") is
  rewritten. Saving a `transcode.*` key bumps
  `playback_input_generation`; note it.

Out of scope whatever R2 says: Dolby Vision Profile 5. It is not a
`CANDIDATES` member, needs RPU side data a generated fixture cannot
carry, and has no CPU baseline. Collapse its three string copies into one
builder (no behaviour change) and leave the rest.

### 6.4 Milestones

**C-M0 — the census.** Signals, baseline, measures, the admin trigger,
admission, the System row, storage.
*Acceptance:* unit tests for the grid and highlight measures on synthetic
frames; a test that the census refuses to start while a foreground
session holds the slot and stops when asked; selection and every filter
string on every node unchanged (the argv goldens do not move); GPT prompt
§9.2 returns four per-node tables and the stills. **Those go to Paul for
R2. Stop here until he rules.**

**C-M1 — tidy with no behaviour change.** One builder for the tonemapx
string.
*Acceptance:* argv goldens unchanged.

**C-M2 — align per R2.** Only graphs the census showed can start on some
node. Executed boot probe with incumbent fallback, identity per §6.3,
telemetry widened. Three tests that must be able to fail: (1) option
listed, graph execution fails → the incumbent string is selected and the
report says so (selecting the new graph anyway fails the test); (2)
filter argv changed with the plan digest held constant → the encoded-VOD
identity differs; (3) with the plan version, ffmpeg build, renderer, source and peak all
held constant, resolve one plan with the aligned graph and one with the
incumbent fallback → their shared recipe hashes differ, and neither is
served the other's cached rendition. A mutant that only bumps
`RESOLVED_TRANSCODE_PLAN_VERSION` must fail (3). Plus one structural test:
the argv for a graph is produced from the resolved configuration value,
so changing the value changes both the hash and the string.
*Acceptance:* argv goldens updated
(`tests/playback/decoder-selection-m0-args.json:20-75`,
`plurx-core/src/transcode/mod.rs:2880, 2973-2993, 3037+`,
`pipeline.rs:785-805, 920-936, 1010-1020, 1214`,
`pipeprobe.rs:1818-1840, 1879-1958`, `ffmpeg.rs:3107, 5615`,
`tests/web/settings-sections.test.js:1519-1533`,
`scripts/decoder-media-baseline:172-174`); a test that a binary lacking
the option gets today's string; a re-run census on the node that selects
the changed graph (through GPT — this check is hardware-only, not CI, and
the PR says so).

**C-M3 — selection per R2,** if he asks for it.
*Acceptance:* unit tests for the selection table; with the setting at its
default every node selects what it selects today.

## 7. Key decisions and why

1. **Encodes only.** An encode's GOP is ours; a copy's is the disc's. The
   copy path's frame-drop history is in the constants' own comments.
2. **8 s, and only three roles.** The publication clock was built with
   24 s of slack and a 48 s steady reserve. Eight keeps the slack above
   the producer's own budget and leaves the reserve alone; smaller values
   are a different system and need their own invariants changed first.
3. **Promote the seam that exists.** The small-bootstrap policy, its
   catch-up and its "steady state intact" rule are already in the tree
   behind `#[cfg(test)]`. A new variant is less code and less risk than a
   new mechanism.
4. **Part B changes how many reads, never which kind or when.** Two of
   rev 1's "duplicates" were a revalidation and a measurement. Each
   removal here names why the second read cannot see anything the first
   did not.
5. **Measure the tone-map before choosing its look.** The S-07 scar: a
   peak that was silently 10 for every title because nothing compared the
   output to anything. And the review's scar: rev 1 picked an operator
   the recorded plan had already argued against.

## 8. How to build it

### 8.1 Lifecycle (Paul's rules, 2026-09-07 and 2026-10-04)

One branch, one draft PR to main (`WIP:` title prefix on Forgejo), proper
commits per milestone. Fast local lane only while building. When a PR's
parts are in: one adversarial agent review of the whole PR, implement its
findings, merge current main in once, run the full suite **once**, re-run
only what failed, then merge the PR yourself. Work in your own clone from
`forge.lan`, never in Paul's checkout. Keep the status page and
`STATUS.md` current as you go.

Three PRs, because three things gate differently:

1. **B-M1 alone** — the corrected log line, the split and the counting
   test. It has to be deployed and measured as Part B's baseline before
   the reductions land (§5.4).
2. **B-M2, B-M3, C-M0, C-M1** — ready now.
3. **Part A** — approved. Start with A-M0; A-M1 does not start until its
   receipt names the bootstrap. C-M2 and C-M3
   wait for R2 and follow separately. Do not hold PR 2 open for either.

**As executed (2026-10-05).** Paul's later instruction replaced two parts
of this: the gate before merge is the fast lane, run once, with only
failures re-run (not the full suite); and work is batched into one larger
PR. B-M1 therefore lands as its own commit inside the batched PR, and
§9.1's baseline arm deploys that commit's sha rather than a separate merge.

### 8.2 Repo rules that will bite

- New documents go in their subject folder with an index row in
  `docs/README.md` in the same commit
  (`tests/operations/test_docs_index.py`); it also rejects any reference
  to a `docs/` path that does not exist. This handoff lands under
  `docs/streaming/` with its row, and
  `PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md` §4.4 points at it.
- A corrective commit touching `crates/` needs a
  `validation/regressions.d` row.
- A new web file needs the file, a `WEB_ASSETS` row, a shell tag and a
  `WEB-SHELL-LAYOUT.md` row in one commit. `tests/ui-structure.golden` is
  regenerated once, by whichever open PR lands last.
- No client source change is planned. If the Apple thresholds in §4.3 do
  need one, the build numbers move (`validation/mobile_versions.py`).
- Establish the Rust 1.97.1 compile loop before editing
  (`docs/DEVELOPMENT_PIPELINE.md` §4).

### 8.3 Main has moved, and what is open (2026-10-05 00:10 UTC)

Main is `70b72f3be`. Since `16f002b70` it gained #811 (repeated HEVC
sample description) and #806 (Jellyfin on the standard port). Astra
checked that the create handler, the transcode manager and publication
paths, the core identity and encoder code, pipeprobe, the ffmpeg probes,
the playback-control actor and the governing startup plan are unchanged
between the two, so the anchors into those hold. **`plurx-core/src/fmp4.rs`,
`plurxd/src/copyseg.rs`, `plurxd/src/main.rs` and
`tests/playback/rolling-producer-owners.toml` did change** — treat every
line number this document gives into those four as approximate and find
the symbol.

| PR | Touches | Coordinate on |
|---|---|---|
| #810 main-merge defects | `web/pages/settings-developer.js`, `http/developer.rs`, `tests/web/settings-sections.test.js`, `docs/OPERATIONS.md` | Developer cards; golden regeneration |
| #807 progressive fence grace | `http/stream.rs`, `serving_fence.rs` | none expected |
| #808 Jellyfin facade gaps | `http/jellyfin.rs` | Jellyfin creates enter through `create_for_compatibility`; re-check §5.1 after it lands |
| #812 Apple system audio hold | Apple client sources | none, unless §4.3's Apple thresholds turn out to need a client change — then the build numbers collide |

#806 has landed. #809 (sharing) is no longer open; re-check §5.1 against
whatever it changed on the create path.

Merge main before your single run, and say in the PR which of these had
landed.

## 9. Prompts for the GPT session

Hand these to Paul to paste; you cannot run them.

### 9.1 Create-path measurement (B-M4) — two builds

> This compares two server builds. Do it in two sittings and do not mix
> them with any Developer switch change.
> **Arm 1, baseline build `<baseline sha>`** on all four nodes: confirm
> `plurx_build_info` on media1 shows it. With no other viewers on media1,
> from Safari on the Mac start each of these 20 times, cold (stop, wait
> 30 s): (a) an HEVC remux with no VOD index; (b) a title with a VOD
> index; (c) a title already playing, reopened at a new position. After
> each start copy media1's `create startup read budget finished` log
> line.
> **Arm 2, candidate build `<candidate sha>`**: deploy it, confirm
> `plurx_build_info`, and repeat exactly, same titles and same order.
> Return one table: arm, build id, shape, authority reads, writes, handler
> ms, store ms (median and range). If both arms show the same build id,
> say so and stop — that run is not valid.

### 9.2 Tone-map census (C-M0)

> Build `<sha>` is on all four nodes. On each node in turn, with no
> viewers on it: Settings → System → "HDR tone-map" → run the census and
> wait for "finished". Return, per node, the whole `tone_map` object from
> `GET /api/v1/system` as an admin and the saved stills. Then play one
> HDR10 title with MaxCLL ≥ 4000 to Chrome, pinned to each node in turn,
> pause at exactly 00:05:00, and screenshot the frame, with the session's
> `pipeline=` log line.

### 9.3 Lab arms (A-M0)

> In the repo at `<sha>` on the Mac, run `scripts/playback-startup-lab`
> for each of these five arms — advertised target / bootstrap, both in
> seconds: **16 / 48** (control), **8 / 24**, **8 / 32**, 6 / 18, 4 / 12.
> Run every arm in Safari native and in hls.js, 30 minutes each at 1.05×,
> 1× and 2× growth. Both target-8 arms are required at every growth rate
> on both players; a run with only one of them is not acceptable. Return
> the JSON receipts, and state the decision the receipt supports, exactly
> one of: "8 / 24 clean at every rate → B = 24 000"; "8 / 24 not clean,
> 8 / 32 clean at every rate → B = 32 000"; "neither clean → Part A is not
> built". Then open the same fixture URLs on the Apple TV test build and
> report stalls and position jumps per arm over ten minutes.

### 9.4 QSV segmentation proof (A-M2b)

> On lab4 with no viewers, at the A-M2 branch commit `<sha>`, run the A-M2 rolling
> segmentation test with the encoder set to QSV (the command is in the PR
> description) against its four frame-rate fixtures, the long-audio-tail
> fixture and the short fixture. Return the test output and, for each
> fixture, the list of EXTINF values including the last.

### 9.5 Short-target measurement (A-M3) — one build, switch off against on

> Build `<candidate sha>` is on all four nodes. Use one title that needs a
> rolling 1080p H.264 SDR transcode (confirm "rolling" and "transcode" in
> Playback info). No other viewers on media1.
> (1) Settings → Developer → "Short rolling target (encodes)" off. From
> Safari on the Mac and from the Apple TV, start the title 10 times, cold.
> For each start record click-to-first-frame, the first playlist's
> `#EXT-X-TARGETDURATION` (save the first and second playlist responses),
> and media1's log line with
> `phase = "writer_inventory_to_first_snapshot"`.
> (2) Turn the switch on and repeat, alternating off and on.
> (3) Switch on, on Apple TV and Android TV: play 30 minutes; pause 60 s
> and resume; pause 10 minutes and resume, noting whether it resumed in
> place or reopened; seek five times in ten seconds; play at 0.5×, 2× and
> 4× for five minutes each; play the last two minutes of the title to its
> end. Report any stall, forward skip, error overlay, −12888 in the device
> log, or `insufficient` / `rolling_window_budget_exhausted` in the node
> log.
> Return a table: client, switch, click-to-frame p50/p95, target in the
> first and second playlist, media seconds at first snapshot.
