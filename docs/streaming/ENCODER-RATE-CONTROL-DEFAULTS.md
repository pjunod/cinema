# Encoder rate-control defaults — flip a family to quality mode only on evidence

**Status:** implementation in progress — M1, M2 and M5 complete; M3/M4 need
fleet acceptance captures · **Executes:** Q1 / F-stream-1 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 · **Implementation base:** `main` @ `21eab120`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read the review's §3.1 row Q1 and the assessment's Q1 / F-stream-1 rows
([ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
first, then §4 of
[PERF2-PLAN.md](../performance/PERF2-PLAN.md) (N1), which built the machinery
this plan reuses. The whole plan is one draft PR into `main`; milestones are
logical commits and Execution-log rows under the current workboard protocol.
Every `file:line` below was first read at `88a3957a`
and is marked **re-verify at build time**. If a step seems to require
changing `EffectiveRateControl::recipe_value()`'s VBR spelling, the HDR10
grade's forced VBR, or the `-maxrate 1.5× / -bufsize 2×` bounds, stop and
flag it: those are recorded decisions, not defaults.

**Correction to the review (narrowing, not reversal):** the review treats
"QSV already swept at 22" as evidence the QSV default can flip. The 2026-08-14
sweep *qualified* quality mode against non-regression gates (VMAF not lower,
easy-clip bytes not higher, speed within 10 %) on a two-clip 1080p SDR
corpus; CHANGELOG (Unreleased, "Performance II N1") records that easy bytes
fell by 188 bytes and the hard clip grew 0.53 %, and says it "does not claim
the plan's expected 10–35 % typical-content savings". That is proof the mode
is *safe* on media1, not proof it is *better*. Flipping a family default moves
every transcode recipe hash on that family, so it needs a measured benefit as
well as a passed gate. §5.3 defines the benefit gate.

## 1. Objective

1. Keep `RateMode::Bitrate` as the effective policy for every family that
   has not been compared, byte-for-byte, so no cache key and no argv baseline
   moves without a decision.
2. Produce, per encoder family, one artefact from `scripts/bench
   rate-control` that compares CRF/QVBR against ABR on a corpus wide enough
   to call representative, with the benefit stated in bytes and VMAF.
3. Let a family's *code default* become `Quality` only when that artefact
   passes both the existing non-regression gates and a benefit gate, while
   the replicated operator pair (`transcode.rate_mode`, `transcode.quality`)
   keeps overriding either way.
4. Leave the HDR10 grade on its separately qualified VBR, and leave every VBV
   bound where it is. Do **not** cap `maxrate` at 1.2× the source bitrate:
   a 3 Mb/s HEVC or AV1 source re-encoded to H.264 legitimately needs more
   than 3.6 Mb/s, and the source average is not a peak.

## 2. Contract today

Requested mode is a replicated pair; the effective value is node-local and
validated before it may reach argv or a recipe.

```text
 settings: transcode.rate_mode / transcode.quality   (replicated pair)
                     |
                     v   2 s refresh loop, node-local 15-frame probe
        RateControlSnapshot { requested_mode, requested_quality, quality_rc }
                     |
                     v   effective_for(encoder)
            EffectiveRateControl::{Vbr | Qvbr{quality}}
                     |
          +----------+-----------+
          v                      v
   Encoder::encode_args_for   plan_digest "rate_control" field -> recipe hash
```

- [`encoder.rs:105-110`](../../crates/plurx-core/src/transcode/encoder.rs)
  — `enum RateMode { #[default] Bitrate, Quality }`. The doc comment above
  it: "This value is never allowed into a cache recipe."
- `encoder.rs:135-142` — `enum EffectiveRateControl { #[default] Vbr,
  Qvbr { quality: u8 } }`; `recipe_value()` at `:147-154` returns the
  literal `vbr:maxrate1.5x:bufsize2x` or `qvbr:q{quality}:maxrate1.5x:bufsize2x`
  with the comment "The VBR spelling is the pre-N1 literal and must never
  change".
- `encoder.rs:189-199` — `default_quality()`: QSV 22 (media1 D5 sweep),
  Software/NVENC/VA-API 23, VideoToolbox 65; "the other families remain
  candidates until the same corpus runs on hardware that can select them".
- `encoder.rs:384-479` — `encode_args_for`: Software adds `-crf q` and drops
  `-b:v` only in the `Qvbr` arm; NVENC `-rc vbr -cq q`, VideoToolbox
  `-q:v q`, VA-API `-rc_mode QVBR -global_quality q`, QSV `-global_quality q`;
  all keep `-maxrate {kbps*3/2}k -bufsize {kbps*2}k`. The doc comment at
  `:376-383` states the HDR10 grade "is always bitrate-bounded VBR" and why
  (`-crf 23` means a different bitrate to x265; the sweep was H.264 only).
- [`transcode.rs:12552-12562`](../../crates/plurxd/src/transcode.rs) —

  ```rust
  fn effective_for(self, encoder: Encoder) -> EffectiveRateControl {
      if self.requested_mode == RateMode::Quality && self.quality_rc.supported_by(encoder) {
          EffectiveRateControl::Qvbr {
              quality: self.requested_quality.unwrap_or_else(|| encoder.default_quality()),
          }
      } else {
          EffectiveRateControl::Vbr
      }
  }
  ```

- `transcode.rs:12503-12518` — `normalize_rate_control_request`: an absent
  or empty stored mode becomes `RateMode::default()` (Bitrate); an explicit
  `bitrate` becomes the same value. **Today the code cannot tell "operator
  never chose" from "operator chose bitrate".** §3.1 depends on that.
- `transcode.rs:14599-14603` — `options_for_tone_map` forces
  `EffectiveRateControl::Vbr` for the HDR10 grade before `encode_args_for`
  does it again. Two independent enforcements; both stay.
- `transcode.rs:19620-19637` — the pair is read with
  `get_setting_pair(keys::TRANSCODE_RATE_MODE, keys::TRANSCODE_QUALITY)`
  ([`store/mod.rs:1537,1540`](../../crates/plurx-core/src/store/mod.rs):
  `"transcode.rate_mode"`, `"transcode.quality"`); the 2 s refresh loop is
  `:19681-19700`.
- `transcode.rs:26619-26627` — `bitrate_for_height`: 2160→20000,
  1080→8000, 720→4000, 480→2000, else 1200 kbps; never reads
  `file.bitrate`. Under `Qvbr` these become ceilings (`-maxrate 1.5×`), not
  targets.
- [`decode.rs:2074-2077`](../../crates/plurx-core/src/transcode/decode.rs) —
  `plan_digest` feeds `"rate_control"` with `recipe_value()`; the plan digest
  enters `Recipe::hash` ([`recipe.rs:96-132`](../../crates/plurx-core/src/transcode/recipe.rs))
  under `CACHE_RECIPE_VERSION = 3` (`recipe.rs:42`). The golden fixture
  `planned_v3_recipe_hash_is_a_golden_fixture` (`recipe.rs:472-478`) hashes
  `TranscodeOptions::default()`, whose rate control is `Vbr`
  ([`mod.rs:900`](../../crates/plurx-core/src/transcode/mod.rs)).
- Offline packages persist the effective value once, at creation
  (`snapshot_value()`, `encoder.rs:157-162`; SQLite v18 / replicated v5 per
  [OPERATIONS.md](../OPERATIONS.md) "N1 adds two admin runtime settings").
- Harness: [`scripts/bench`](../../scripts/bench) `rate-control` — modes
  `vbr` (smoke) or `vbr,qvbr` (full); acceptance constants at `:104-112`
  (`vmaf_v0.6.1`, subsample 1, 10 s peak window, owner-ratified
  2026-08-12); gates in `evaluate_rate_control` (`:1034-1176`):
  `ladder_identity_mismatch`, `encoder_identity_mismatch`,
  `duration_mismatch`, `peak_evidence_invalid`, `advertised_peak_exceeded`,
  `vmaf_regression`, `easy_bytes_regression`, `speed_regression` (p10 speed
  below 90 % of VBR). Corpora:
  [`scripts/perf2-rate-control-smoke-corpus.json`](../../scripts/perf2-rate-control-smoke-corpus.json)
  and [`scripts/perf2-rate-control-n1-corpus.json`](../../scripts/perf2-rate-control-n1-corpus.json)
  — each two 1080p SDR fixtures (`easy-1080p-h264`, `hard-1080p-grain`).
  The harness requires `dynamic_range: sdr` on every full-corpus fixture.

Fleet families that can run the harness (read-only inventory,
[DECODER_SELECTION_RECOVERY_STATUS.md](../DECODER_SELECTION_RECOVERY_STATUS.md)
"FFmpeg and client qualification gaps"): QSV on media1, lab3, lab4;
software everywhere; VideoToolbox only on the Apple build host, which runs
no daemon. No NVENC or VA-API node is recorded — those families cannot be
compared until one exists, and their default stays Bitrate.

## 3. Change

### 3.1 A per-family code default, distinct from the operator's pair

Add `Encoder::default_rate_mode(self) -> RateMode` returning `Bitrate` for
every family at first. `RateControlSnapshot.requested_mode` becomes
`Option<RateMode>`: `None` when the stored setting is absent or empty,
`Some(Bitrate)` when the operator wrote `bitrate`. `effective_for` resolves
`requested_mode.unwrap_or(encoder.default_rate_mode())` and keeps every
other rule (a family whose `quality_rc` bit is false stays `Vbr`; explicit
`transcode.quality` still overrides `default_quality()`).

Reason: the flip must be per family and must not disturb an operator who
chose bitrate on purpose. Without the third state, changing
`RateMode::default()` would also override that operator's choice.

`GET /api/v1/settings` keeps returning the stored pair; `GET /api/v1/system`
`encoders.quality_rc` gains `default_rate_mode` per family so the Developer
page can print "family default: quality (qualified 2026-MM-DD, artefact
sha256 …)". No new setting key: the existing pair is the switch, and it is
already replicated and surfaced.

### 3.2 The corpus grows before any comparison counts

Add fixtures to `scripts/bench` `FIXTURES` (`:55-97`) and a new
`scripts/perf2-rate-control-n2-corpus.json` (`purpose: n1_acceptance`, the
schema the harness already validates) with balanced `easy`/`hard` halves:

| identity | why it is here |
|---|---|
| `easy-1080p-h264` | existing baseline |
| `easy-1080p-animation` | flat regions, hard edges — where CRF spends least |
| `easy-720p-web` | low-detail source; `-b:v` target overspends here |
| `hard-1080p-grain` | existing; rate-control overshoot |
| `hard-1080p-dark-gradient` | banding under quantiser pumping |
| `hard-1080p-sport` | fast motion at 59.94; VBV binds |

All SDR, because the harness requires it and because the HDR10 grade is out
of scope (its rate control is a separate qualified policy). A tone-mapped SDR
*output* of an HDR source is admissible once the **scorer** can handle it, not
merely once the manifest can spell it. `score_vmaf` builds its graph with no
tone-map on either leg — `format=yuv420p` converts depth and chroma, not
transfer or primaries — so an admitted `dynamic_range: hdr10` fixture would
have a PQ reference compared against BT.709 SDR output. Both modes would then
score the same meaningless number: `vmaf_regression` and `quality_benefit_path`
would compare noise while the byte gates kept working, which is a gate passing
for the wrong reason rather than one failing loudly. `load_rate_control_corpus`
therefore refuses `hdr10` and names the missing tone-map. Adding the tone-map
is the harness change with its own PR, and it is required before the "every HDR
tone-map session goes through this" claim in the appendix can be measured.

**Fixture reproducibility is a property of the recipe, not of the manifest.**
`scripts/bench fixtures` skips a file that already exists, so a pinned
`reference_sha256` is only reachable on a host that does not already hold the
bytes when the recipe produces the same bytes every time. Three things are
needed and the corpus originally had only the first two, and only on four of
its six fixtures:

1. **Bit-exact muxing.** Without `-map_metadata -1 -fflags +bitexact
   -flags:v +bitexact -flags:a +bitexact`, Matroska writes a random SegmentUID
   and Lavf tags, so two runs of the same recipe differ.
2. **A seed on anything random.** The `grainy` fixture's grain comes from
   `noise=alls=40:allf=t+u` with no `all_seed`, which changes the *pixels* as
   well as the container.
3. **A fixed thread count.** libx264 partitions frame threads by the host's
   core count, so the same bit-exact recipe produces different bytes on a
   16-core and a 2-core machine. This is why even the four bit-exact fixtures'
   pinned hashes could not be reproduced on another host. `-threads 1` closes
   it, measured: the same recipe under `taskset -c 0,1` and with all 16 cores
   available produces one hash.

All six fixtures the pinned manifests reference now carry all three, and both
`scripts/perf2-rate-control-n1-corpus.json` and the n2 manifest are re-pinned
together from that recipe. The pins remain tied to the encoder build — they
were regenerated on ffmpeg 8.0.1-3ubuntu2 (the controller's accepted build) —
and nothing in the harness can make a hash portable across x264 versions; what
is now portable is everything the controller actually varies.

### 3.3 The benefit gate

A family may flip only when the full `vbr,qvbr` run over the n2 corpus:

- passes every existing gate (unchanged constants), and
- shows, over the whole corpus, either bytes down ≥ 10 % at VMAF not lower
  on any fixture, or VMAF up ≥ 1.0 on every `hard` fixture at bytes not
  higher on any `easy` fixture.

The 10 % is the low end of the range PERF2-PLAN §4 quoted and the sweep did
not reach on two clips; it is the threshold at which moving every cache key
buys something a viewer or a disk notices. Numbers below it leave the family
on Bitrate and record the artefact so the question is closed, not open.

### 3.4 What the flip changes, and what invalidates

- Recipe field `rate_control` moves from `vbr:maxrate1.5x:bufsize2x` to
  `qvbr:q{q}:maxrate1.5x:bufsize2x` for new SDR transcodes on the flipped
  family. Old shared-cache entries stop matching and age out by LRU
  ("invalidation is by mismatch, never by deletion", `recipe.rs:17-22`).
  `CACHE_RECIPE_VERSION` stays 3: nothing about the hash construction
  changes, only a field's value on some inputs.
- Encoded-VOD renditions: the recipe carries the argv, so flipped-family
  renditions get new keys; existing immutable renditions remain valid under
  their old keys and are never rewritten.
- Offline packages already created keep their persisted `vbr` snapshot.
- The golden fixture and `the_sdr_argument_list_for_a_profile5_source_is_unchanged`
  (`mod.rs:2877`) keep passing because they construct `Vbr` explicitly;
  a test that builds options through `effective_rate_control()` with an
  unset pair on a flipped family must be updated to expect `Qvbr`, and that
  update is the visible record of the flip.
- Fragment indexes (copy path) are untouched: rate control is not part of a
  copy recipe.

## 4. Guardrails (non-goals)

- **HDR10 grade stays VBR** — both enforcements (`transcode.rs:14599`,
  `encoder.rs:392`) untouched; the review's assessment requires it and the
  x265/x264 CRF scales differ.
- **No universal `maxrate = 1.2 × source`** — the assessment's F-stream-1
  row rejects it; source average is not a cross-codec peak cap.
- **VBV bounds unchanged** — `-maxrate 1.5×`, `-bufsize 2×` are the measured
  model in `encoder.rs:328-346`; the harness's `advertised_peak_exceeded`
  gate is defined against them.
- **No in-code feature gate.** The operator pair is the switch; the
  per-family default is a code constant changed by a PR that cites its
  artefact.
- **Requested values never enter a recipe** (`encoder.rs:100-104`); only
  `EffectiveRateControl` does. §3.1 does not change that.
- **Do not flip NVENC, VA-API or VideoToolbox** without a node that runs the
  harness through a deployed `plurxd`; a passing 15-frame boot probe is
  capability evidence, not calibration (OPERATIONS.md, same section).
- **VMAF covers picture, not audio or timing** (review §7.3). The harness's
  `duration_mismatch` and speed gates stay; nothing here judges audio.

## 5. Milestones

### 5.1 Tri-state request and per-family default (no behaviour change)

`Option<RateMode>` in `RateControlSnapshot`; `Encoder::default_rate_mode()`
returning `Bitrate` everywhere; `/system` reports it. Tests: an unset pair
resolves to `Vbr` on every family; an explicit `bitrate` resolves to `Vbr`;
`quality` with `quality_rc` true resolves to `Qvbr` with the family default;
the golden recipe hash is unchanged.

Acceptance: `cargo test -p plurxd rate_control` and `cargo test -p plurx-core
recipe` green; `curl -s http://media1:32400/api/v1/system | jq
.encoders.quality_rc` shows `default_rate_mode: "bitrate"` for every family
after deploy.

### 5.2 Corpus and harness extension

New fixtures in `scripts/bench fixtures`; n2 corpus manifest with pinned
SHA-256s; harness validates `output_grade` on a fixture and refuses an HDR
source outright while `score_vmaf` has no tone-map (so an HDR10-rung capture
can never be scored against an SDR reference by accident). Python gate:
`make operations-check` (`Makefile:130-131`, which runs
[`tests/operations/test_bench_rate_control.py`](../../tests/operations/test_bench_rate_control.py))
plus a `scripts/bench rate-control --modes vbr --corpus
scripts/perf2-rate-control-n2-corpus.json` smoke on media1.

Acceptance: the smoke JSON reports six fixtures, `purpose:
"n1_acceptance"`, no `harness_error`, and the fixture identities above.

### 5.3 The QSV comparison on media1

Full `--modes vbr,qvbr` over the n2 corpus with media1 reserved
(OPERATIONS.md "Rate-control acceptance captures production, then scores
offline"), server hashes diffed, artefact SHA-256 recorded in the PR body.
Apply §3.3. If it passes, the PR also flips `default_rate_mode(Qsv)` to
`Quality` and updates the tests that expected `Vbr` for an unset pair.

GPT prompt (device/fleet access required):

```text
On the controller with the accepted libvmaf ffmpeg, with media1 idle:
1. scripts/bench fixtures --dir bench-media   (adds the n2 fixtures)
2. copy bench-media/ to media1's fixture library path, rescan, and diff
   sha256sum lists as OPERATIONS.md "Rate-control acceptance" describes.
3. scripts/bench rate-control --base http://media1:32400 --token $T \
     --library $LIB --corpus scripts/perf2-rate-control-n2-corpus.json \
     --modes vbr,qvbr --server-sha256-manifest out/media1-perf2.sha256 \
     --vmaf-ffmpeg <scorer> --vmaf-model vmaf_v0.6.1 \
     --json out/rate-control-n2-qsv.json
4. Report: per fixture, bytes and VMAF for vbr and qvbr; the failures
   array; sha256 of the JSON; and journalctl lines showing
   `-global_quality 22` on the qvbr sessions.
```

Acceptance: `failures: []` in the artefact **and** the §3.3 benefit
condition, or a recorded "stays Bitrate" with the numbers.

### 5.4 The software comparison

Same run with `PLURX_HWACCEL=` forcing software on a lab node whose CPU is
representative (lab4; the harness records the production encoder identity
and refuses a mid-run change). Software is the floor every node has, so this
is the flip with the widest blast radius; it needs its own artefact even
though x264 CRF-with-VBV is the best-understood mode of the five.

Acceptance: as 5.3, for `Encoder::Software`.

### 5.5 Close the families without acceptance evidence

Record in this document's §7 and in OPERATIONS.md that NVENC, VA-API and
VideoToolbox defaults remain Bitrate with the observation that would unpark
each: a node whose `plurx_sessions_total{encoder="nvenc"|"vaapi"|
"videotoolbox"}` is non-zero for a week. That metric exists
([`telemetry.rs:221-225`](../../crates/plurxd/src/telemetry.rs)).

Acceptance: `curl -s http://<node>:32400/metrics | grep
plurx_sessions_total` across media1, lab3, lab4, lab6 shows which families
actually serve sessions; the doc row matches.

## 6. Verification and rollout

- Fast lane: `make unit` (`Makefile:44-46`) covers the Rust changes; the
  focused commands are `cargo test -p plurxd rate_control`, `cargo test -p
  plurx-core recipe`, `cargo test -p plurx-core encoder`.
- `make operations-check` covers the `scripts/bench` contract tests.
- The one plan PR carries each flip's artefact path and SHA-256, corpus manifest
  SHA-256, deployed build string, the per-fixture table, and the sentence
  "every SDR transcode recipe on <family> changes key; caches refill by
  mismatch".
- Rollout order inside the one plan PR: 5.1 → 5.2 → 5.3 → 5.4; 5.5 is
  documentation.
- Production counter (review §5.3 rule): after a flip, watch
  `plurx_cache_serves_total` (existing) for the expected miss wave and
  `plurx_sessions_total{encoder}` for the family; alert owner: Paul; window:
  seven days; the expected-demand denominator is sessions on that family.
- Rollback: revert the one-line default; new sessions return to the old
  recipe key, whose entries may still be cached.

## 7. Evidence and decisions

### 7.1 Read-only fleet census — 2026-09-21 06:54 UTC

All four Linux daemons ran OCI revision
`882862e887fa26a064be0de30bf9797698af8e84`. The evidence was read-only:
`/api/v1/server`, the boot capability line in `docker logs plurxd`, and
`/metrics`; no setting, container, library or media file changed.

| Deployment host | Selected family / boot capability | Process-lifetime encoded-session counters | Decision |
|---|---|---|---|
| `nynuc` (media1) | QSV selected; QSV, VA-API and software quality probes passed | QSV 0, VA-API 0, software 0, NVENC 0, VideoToolbox 0 | QSV stays Bitrate: capability is not the n2 comparison. |
| `nuc4` (lab4) | QSV selected; QSV, VA-API and software quality probes passed | all five encoded families 0 | QSV and software stay Bitrate pending their separate captures. |
| `nuc3` (lab3 learner) | QSV selected; QSV, VA-API and software quality probes passed | all five encoded families 0 | QSV stays Bitrate. `/api/v1/server` returned 503 on the learner; the OCI revision label, boot log and metrics remained readable. |
| `m6` (lab6) | VA-API selected; VA-API and software quality probes passed; QSV and NVENC validation failed | VA-API 0, software 0, NVENC 0, VideoToolbox 0; copy 3 and VOD 1 | The old plan statement that no VA-API node existed is superseded. VA-API still stays Bitrate because the new process has no encoded session evidence, much less a week or an n2 comparison. |

The counters had restarted with the 2026-09-21 deployment, about 2.5 hours
before this read. They are a current-process observation, not a seven-day
history. NVENC is unusable on these Linux nodes, VideoToolbox is absent, and
the Apple build host runs no daemon. The observation that unparks either
family remains a deployed node with a non-zero family counter for a week;
VA-API additionally needs its own n2 comparison now that a selectable node
exists.

### 7.2 Safe implementation boundary

- M1 is behavior-neutral: an absent setting is now distinct from explicit
  `bitrate`, every `Encoder::default_rate_mode()` remains Bitrate, and
  `/system` reports all five defaults beside the quality capability verdicts.
  It is behavior-neutral including in the *speculative pretranscode dedupe
  key*, which the first implementation moved. `pretranscode_policy_generation_for`
  hashed `requested:family_default` for an unset pair where it had hashed
  `requested:bitrate`; that string is durable per-queue-row state and a
  mismatch is a hard `cancel_job(.., "policy_changed")`, not a yield, so every
  queued speculative row on every node would have been cancelled and
  rediscovered at deploy — and again mid-boot on every restart, because the
  manager's first snapshot is `RateControlSnapshot::bitrate` and
  `initialize_rate_control` then republishes the absent pair as `None`. Unset
  and explicit `bitrate` resolve to the same effective policy on every family,
  so they spell the same thing. A PR that flips a family default moves this
  spelling deliberately, with the artefact §3.4 requires, and
  `an_unset_rate_control_pair_keeps_the_explicit_bitrate_policy_generation`
  pins the durable value so it cannot move by accident.
- M2 adds the balanced six-fixture n2 manifest, reproducible generation for
  **all six** fixtures it references, source/output-grade validation, and the
  exact §3.3 benefit gate. The first implementation gave the four new fixtures
  bit-exact muxing and left the two inherited ones, `1080p-h264` and `grainy`,
  without it — and `grainy`'s noise without a seed — so neither could be
  regenerated to the hash the manifest pinned; and no fixture pinned its thread
  count, so even the bit-exact four were reproducible only on a host with the
  generating machine's core count. Both are fixed and both manifests are
  re-pinned from the corrected recipe. Measured on nuc3 (ffmpeg
  8.0.1-3ubuntu2): before the fix, two runs of `1080p-h264` gave
  `b9906d6b…`/`9f4bc7a7…` and two runs of `grainy` gave `a2ceeec5…`/`f772af54…`,
  while the bit-exact `1080p-animation` gave `53a6da75…` twice but
  `f8181814…` under `taskset -c 0,1`. After the fix, three generations of every
  fixture — two at 16 cores and one at 2 — were byte-identical, and those are
  the hashes now pinned. The temporary generated media was removed.
- The media1 smoke and the M3/M4 comparisons were not run. `/srv/bench-media`
  is absent on media1, and the authorized census was read-only; copying media,
  rescanning a production library, changing the replicated rate-control pair,
  or forcing a daemon to software would be a fleet mutation. The PR therefore
  contains no QSV or software default flip.

### 7.3 Recorded decisions

1. Burned subtitles remain outside n2 because a burn changes the encoder's
   input rather than isolating rate control.
2. SDR-source evidence gates only SDR-source defaults. The harness refuses an
   HDR source entirely, naming the missing tone-map, because admitting one
   would score SDR output against a PQ reference — the same wrong number the
   removed guard prevented. `output_grade` is still validated so the manifest
   can record the intent, and admitting `hdr10` is the tone-map PR's to do.
3. `bitrate_for_height`, `-maxrate 1.5×`, and `-bufsize 2×` remain unchanged.
   No source-average bitrate cap is introduced.

---

## Execution log

Executing sessions append one row per logical milestone commit (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Claim | [#414](http://192.168.4.7:3000/noirr/plurx/pulls/414) | Draft plan PR claimed from `main` `21eab120`; board link commit `cab6e764`. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M1 | [#414](http://192.168.4.7:3000/noirr/plurx/pulls/414) | `46fac273`: tri-state request and per-family defaults; all defaults remain Bitrate. Pinned compile, encoder (34 + 1 integration), recipe (12 + 1 integration), and filtered plurxd rate-control (3) checks passed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M2 | [#414](http://192.168.4.7:3000/noirr/plurx/pulls/414) | `2917a84c`: deterministic six-fixture n2 corpus, output-grade contract and benefit gate; 55 focused Python tests passed. Media1 smoke needs the read-write fleet step in §5.2. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M3 | [#414](http://192.168.4.7:3000/noirr/plurx/pulls/414) | needs: reserved media1 n2 `vbr,qvbr` capture under the §5.3 prompt. QSV remains Bitrate. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M4 | [#414](http://192.168.4.7:3000/noirr/plurx/pulls/414) | needs: representative lab4 n2 capture with hardware selection disabled under the §5.4 contract. Software remains Bitrate. |
| 2026-09-22 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1/M2 review fixes | [#414](http://192.168.4.7:3000/noirr/plurx/pulls/414) | All three adversarial-review findings fixed. M1's dedupe key spells an unset pair `requested:bitrate` again; the two inherited fixtures gained bit-exact, seeded generation and every pinned fixture gained `-threads 1`, with both manifests re-pinned from three byte-identical generations across two core counts; and `load_rate_control_corpus` refuses `dynamic_range: hdr10` while `score_vmaf` has no tone-map. Each fix has a test that fails on revert. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M5 | [#414](http://192.168.4.7:3000/noirr/plurx/pulls/414) | Read-only four-node census in §7.1. NVENC and VideoToolbox remain unrunnable; VA-API is selectable on m6 but has zero encoded sessions since restart. All three defaults remain Bitrate. |
