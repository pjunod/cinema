# Encoder rate-control defaults — flip a family to quality mode only on evidence

**Status:** ready for review · **Executes:** Q1 / F-stream-1 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

Read the review's §3.1 row Q1 and the assessment's Q1 / F-stream-1 rows
([ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
first, then §4 of
[PERF2-PLAN.md](../performance/PERF2-PLAN.md) (N1), which built the machinery
this plan reuses. Work the milestones in order; each is one draft PR into
`main` under the fast lane. Every `file:line` below was read at `88a3957a`
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
*output* of an HDR source is admissible once the harness lets a fixture
declare `dynamic_range: hdr10` with `output_grade: sdr`; that is a harness
change with its own PR (§5.2) and is required before the "every HDR tone-map
session goes through this" claim in the appendix can be measured.

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
SHA-256s; harness accepts `output_grade` on a fixture and refuses an HDR
source whose output grade is not `sdr` (so an HDR10-rung capture can never
be scored against an SDR reference by accident). Python gate:
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

### 5.5 Close the un-runnable families

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
- Each flip PR body carries: artefact path and SHA-256, corpus manifest
  SHA-256, deployed build string, the per-fixture table, and the sentence
  "every SDR transcode recipe on <family> changes key; caches refill by
  mismatch".
- Rollout order: 5.1 → 5.2 → 5.3 → 5.4; 5.5 is documentation. One PR each.
- Production counter (review §5.3 rule): after a flip, watch
  `plurx_cache_serves_total` (existing) for the expected miss wave and
  `plurx_sessions_total{encoder}` for the family; alert owner: Paul; window:
  seven days; the expected-demand denominator is sessions on that family.
- Rollback: revert the one-line default; new sessions return to the old
  recipe key, whose entries may still be cached.

## 7. Open questions

1. Whether the n2 corpus should include a burned-subtitle fixture: a burn
   changes the encoder's input, not its rate control, so it is excluded
   here; say so if the reviewer disagrees.
2. The tone-mapped-SDR-output-of-HDR fixture needs the harness change in
   §5.2; whether to gate the QSV flip on it, or accept SDR-source evidence
   for SDR output, is Paul's call. This plan gates on SDR sources only and
   records the HDR-source run as follow-up evidence.
3. Whether a flipped family should also raise `bitrate_for_height` ceilings
   (they become caps, not targets): no — unchanged caps keep
   `Rung.peak_kbps` and every advertised BANDWIDTH honest (see
   [HONEST-MASTER-PLAYLIST.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)).
