# Recording and reminders — implementation status and evidence

**Status:** server and all three clients built on `effort/live-tv-dvr`,
reviewed and qualified, merging to `main`; nothing has touched a tuner yet ·
**Base:** Forgejo `main` at `75edcb44` · **PR:**
[#294](http://192.168.4.7:3000/noirr/plurx/pulls/294) ·
[#295](http://192.168.4.7:3000/noirr/plurx/issues/295) ·
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
| One channel is one tuner, however many programmes come off it | The 8:30 programme's tail pad overlaps the 9:00 programme's head pad. Counting recordings rather than channels would refuse the second one for no reason | `DvrTransport`/`DvrSink`, `back_to_back_airings_on_one_channel_cost_one_tuner` |
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

## Two prerequisites built alongside

The plan pins DVR M0 on the reliability effort's durable guide and M2's
promotion on its same-viewer stray eviction. Neither was on `main` — that
effort is planned but unbuilt — so both were built here, and only those two.

The guide now survives a restart in `cache_dir/live-tv/guide.json`, keeps the
age it already had when it was adopted, and wakes on a cold lineup, a settings
save or a fence change rather than only on a twenty-minute timer. That is also
what makes a 14-day horizon possible at all: a fortnight cannot be fetched in
one refresh, and accumulating across refreshes is only safe once a restart does
not start from nothing.

The stray eviction is a prerequisite rather than a nicety. The capacity dialog
names which recordings hold which tuners and offers to stop one; without the
eviction, a viewer holding an idle session of their own would be told a
recording had their tuner and would stop it to free a slot they were already
occupying.

## Evidence

| Scope | Evidence | State |
|---|---|---|
| `plurx-core` — `dvr.rs`, both Store backends, the contract scenarios | `cargo test -p plurx-core` DVR + `scan::recordings` unit cases and all four contract scenarios green on SQLite; `make cluster-store-check` runs the same four on three voters | built |
| `plurxd` — engine, routes, settings, Developer item, scan | `cargo clippy --workspace --all-targets -D warnings` clean; 97 focused DVR, webhook, schedule, guide and Developer cases green | built |
| Web · Apple · Android | every `make web-check` step green but the pre-existing one below; no Swift or Android toolchain in the build container, so both native clients were verified by reading | built, unproved on a device |
| The repository's own contracts | `make operations-check` 325 green; `validation/mobile_versions.py` clean; `scripts/ui-baseline --self-host --check` 7,912 structural facts across 78 captures match the golden with no console or page errors | built |

### The one adversarial review

Reviewed once, at the point this branch opened its PR to `main`, as
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) requires. Five P1s, ten
P2s and three P3s; all addressed in `170b1e7d`, whose message carries the
disposition of each. The severe one is worth naming here because no existing
test could have caught it: every path a recording owned was built with
`Path::with_extension`, which cuts at the *last* dot — and a basename ends
`… - 7.1 - abcdef01`. Two sub-channels airing a programme called *News* at six
would have written one file. The unit test asserted the `String` basename and
never a built `PathBuf`, so the bug lived entirely in the gap between them.

Three `plurxd` unit tests and one web test were **already red on `main` at
`75edcb44`** and are untouched here: `live_tv_software_hls_argument_baseline_is_stable`,
`live_hls_publishes_short_startup_segments_before_steady_cadence`,
`one_tuner_get_runs_the_full_hls_lifecycle_and_stop_waits_for_cleanup` (all
three want an `ffprobe` the build container does not configure), and
`tests/web/layout-containment.test.js` (whose five offending declarations are
byte-identical in the merge base — verified by running the test against a
pristine `main` checkout).

## What remains unproved

Everything that needs the hardware. The FLEX 4K has never been asked whether
its tier answers `Start=` days out or carries `SeriesID`; no capture has ever
been written; the raw-TS files have never been played back through the real
decision path; and no phone has fired a local notification. §8 of the
implementation plan carries the two prompts for the session that can do it.
