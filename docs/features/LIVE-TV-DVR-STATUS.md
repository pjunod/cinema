# Recording and reminders — implementation status and evidence

**Status:** **merged to `main`** at `aba14096`; the whole fast lane green,
including the first Apple and Android compiles this code has ever had. Nothing
has touched a tuner yet — that pass is
[DVR-HARDWARE-VERIFICATION-PROMPT.md](../clients/DVR-HARDWARE-VERIFICATION-PROMPT.md) ·
**Branch:** `effort/live-tv-dvr` ·
**Base:** Forgejo `main` at `a605d03c` (started at `75edcb44`; `main` moved 138
commits under this branch — 116 excluding merges, in two waves of 115 and 23 —
and both are merged in) ·
**PR:** [#294](http://forge.lan:3000/noirr/plurx/pulls/294) ·
**Issue:** [#295](http://forge.lan:3000/noirr/plurx/issues/295) ·
**Apple build 153 · Android versionCode 94** ·
**Updated:** 2026-09-13

Companion to
[LIVE-TV-DVR-IMPLEMENTATION.md](LIVE-TV-DVR-IMPLEMENTATION.md) (the plan this
executes), [LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md](LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md)
(why each choice), and [../DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md)
(the one-review fast lane) — this is the short answer to *what has shipped,
what it is known not to do, and what remains unproved*.

## What it does

Any guide cell on web, Apple and Android offers **Record · Record series ·
Remind me** beside Watch. The owner node writes the tuner's bytes to a `.ts`
file under the DVR root with a sidecar of guide metadata, the file appears in a
`recordings` library that every node serves through the ordinary VOD path, and
a reminder reaches an in-app overlay on every open client, a local notification
on a phone, and one webhook POST.

Nothing is gated. `dvr.enabled` is a plain setting; the Developer tab says what
should be true first and whether it is, and no enable path reads it.

## The four properties the design is built on

Each was a defect in the plan's first draft, found in review, so each is a
regression test rather than a claim.

| Property | Why it is not obvious | Where it is enforced |
|---|---|---|
| An airing is `(channel_id, airing_start)` for its whole life | Rule expansion runs every 15 s. Without insert-if-absent over a unique index covering *every* state, a rule that keeps matching an airing the viewer skipped resurrects it on the next tick | `dvr_recordings_airing`, `insert_dvr_airing_if_absent`, `a_cancelled_airing_stays_cancelled_however_often_a_rule_matches_it` |
| One channel is one tuner, however many programmes come off it | The 8:30 programme's tail pad overlaps the 9:00 programme's head pad. Counting recordings rather than channels would refuse the second one for no reason | `DvrTransport`/`DvrSink`, `back_to_back_airings_on_one_channel_cost_one_tuner` (adjacent windows) and `overlapping_pads_on_one_channel_still_share_the_tuner` (the padded case this row describes) |
| An attempt is its own file | A fenced old owner may still be draining bytes when the replacement resumes. Appending to the same file is how two processes corrupt one recording | `.a<N>.part` + `create_new`, `finish` concatenates and records the gap |
| A stop and a progress write own disjoint columns | They arrive from different nodes, seconds apart, on the same row | `request_dvr_stop` / `progress_dvr_recording`, `a_stop_request_survives_the_progress_writes_racing_it_and_is_idempotent` |

## What it deliberately does not do

- **Play while recording.** The `.part` file is the seed for it; the VOD path
  must first accept a growing input.
- **File recordings into Shows.** A capture is an airing, not an episode of a
  catalogued series, and guessing which series would be a metadata match this
  library kind exists to avoid.
- **Fetch programme artwork.** The guide's image host is not on the approved
  artwork allowlist, and widening that list is an SSRF decision of its own.
- **Push a reminder to a closed phone.** The phone mirrors the server's armed
  reminders into its own scheduler when the app is open. A reminder created,
  deleted or moved on another device while this phone stays closed is not
  reflected until it next opens. APNs closes that; this is the honest floor.
- **Record protected ATSC 3.0.** The engine cannot open it.

## Two prerequisites, built here and then given back

The plan pins DVR M0 on the reliability effort's durable guide and M2's
promotion on its same-viewer stray eviction. Neither was on `main` when this
started — that effort was planned but unbuilt — so both were built here, and
only those two.

`effort/live-tv-reliability` ([#281](http://forge.lan:3000/noirr/plurx/pulls/281))
has since merged its own implementations of exactly those two things. Two
independent builds of one feature, converging on the same field names, is what
made `live_tv.rs` conflict in thirty-five places on the merge. **The rule
applied throughout: where both sides built the same thing, main wins** — its
version is the one already reviewed, merged and deployed. So `main`'s durable
guide is what ships (`GuideCache::with_store`, `load_persisted`, the
epoch-guarded `persist_guide` rename, `CachedGuide::age_at`, the refresh loop's
wake-on-lineup/settings/fence select), and `main`'s `stray_to_evict`, which
also covers a retired request id in any phase — something this branch's inline
version did not. Main's version also fixed a bug this branch had:
`SnapshotCache::with_wake` here ignored the `Notify` it was handed and made a
fresh one, so the lineup wake it exists to deliver could never arrive.

What this branch keeps is what is genuinely DVR: the 336-hour horizon a series
rule needs to schedule from (and the incremental extension and
`revalidate_guide_day` that fill it, because a fortnight cannot be fetched in
one refresh), the programme identity a rule matches on, and the tuner
accounting that lets a recording hold a transport — the capacity branch counts
`held()` rather than live sessions, so a viewer is told which recordings hold
which tuners and offered a way to stop one.

The stray eviction was a prerequisite rather than a nicety, and remains so:
without it a viewer holding an idle session of their own would be told a
recording had their tuner and would stop it to free a slot they were already
occupying.

## Evidence

Everything below was run **on the merged tree**, not on the branch before it.

| Scope | Evidence | State |
|---|---|---|
| `plurx-core` — `dvr.rs`, both Store backends, the contract scenarios | `cargo test -p plurx-core --lib` 801 green (1 pre-existing, below); all 109 store contract scenarios green on SQLite; `make cluster-store-check` runs the same four DVR scenarios on three voters | built |
| `plurxd` — engine, routes, settings, Developer item, scan | `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test -p plurxd --bin plurxd` 2,224 green (17 pre-existing, below) | built |
| Web · Apple · Android | every `make web-check` step green but the pre-existing one below; no Swift or Android toolchain in the build container, so both native clients were verified by reading | built, unproved on a device |
| The repository's own contracts | `make operations-check` 356 green · `make history-check` ok · `scripts/validate lint` ok, 26 points · `make benchmark-check` 52 green · `validation/mobile_versions.py` clean · `scripts/ui-baseline --self-host --check` 7,966 structural facts across 78 captures match the regenerated golden, no console or page errors | built |

### The one adversarial review

Reviewed once, at the point this branch opened its PR to `main`, as
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) requires. Five P1s, ten
P2s and three P3s; all addressed in `b2c3ac2c`, whose message carries the
disposition of each and whose `validation/regressions.d` row says, finding by
finding, which are pinned by a test and which are not. The severe one is worth
naming here because no existing
test could have caught it: every path a recording owned was built with
`Path::with_extension`, which cuts at the *last* dot — and a basename ends
`… - 7.1 - abcdef01`. Two sub-channels airing a programme called *News* at six
would have written one file. The unit test asserted the `String` basename and
never a built `PathBuf`, so the bug lived entirely in the gap between them.

### The failures, and why none of them is this work

The merged tree's failure set is **exactly `main`'s own, one for one**. Each
was verified by running it against a pristine `main` checkout rather than
asserted:

- thirteen `decode_facts` cases and `playback_control::tests::copy_retry_is_unsupported_only_and_successor_classifier_is_recipe_owned`
  — these want process facilities (sealed exec, `pidfd`, kill domains) the
  build container does not have;
- `live_tv_software_hls_argument_baseline_is_stable`,
  `live_hls_publishes_short_startup_segments_before_steady_cadence` and
  `one_tuner_get_runs_the_full_hls_lifecycle_and_stop_waits_for_cleanup` — all
  three want an `ffprobe` the container does not configure;
- `fmp4::tests::complete_multi_entry_hevc_is_a_typed_validated_structural_refusal`
  — issue [#237](http://forge.lan:3000/noirr/plurx/issues/237);
- `tests/playback/web-control.test.js`'s `an interval past the deadline is
  clamped to it`, whose assertion reads the real wall clock and therefore
  depends on how fast the container ran the preceding 13 seconds of suite.

`tests/web/layout-containment.test.js`, which was red on this branch before the
merge, is **green now**: `main` fixed it in `5ae76988`.

## What remains unproved

### 2026-09-21 scheduler and sink isolation follow-up

Draft PR [#407](http://192.168.4.7:3000/noirr/plurx/pulls/407) implements the
architecture review's L1/L10 correction without changing a setting or wire
shape. The owner-side scheduler, reconciliation pass and reminder sweep now
share one immutable, unclipped guide generation; the public and relay guide
bodies remain capped at 2 MiB. Each recording sink now owns a bounded writer
queue, so a stalled disk ends only that attempt with one of three fixed reason
codes while the tuner reader and healthy sibling sinks continue.

Pinned Rust 1.97.1 compilation and focused in-memory regressions prove the
day-13 scheduling/public-clipping split, late-row reconciliation, bounded
backpressure, sink-local failure, shared overlap bytes, serving-fence
cancellation and settlement-before-assembly. The fleet prompts in the plan
have not run: there is no claim yet about a real 336-hour HDHomeRun guide, NAS
export pause, attempt rollover or owner log. This row remains implementation
evidence, not hardware acceptance.

Everything that needs the hardware. The FLEX 4K has never been asked whether
its tier answers `Start=` days out or carries `SeriesID`; no capture has ever
been written; the raw-TS files have never been played back through the real
decision path; and no phone has fired a local notification.
[DVR-HARDWARE-VERIFICATION-PROMPT.md](../clients/DVR-HARDWARE-VERIFICATION-PROMPT.md)
is the hand-off for the session that can: it carries §8's two prompts and the
settings, owner-node and client details a reader needs to run them without
filing a defect that is not one.
