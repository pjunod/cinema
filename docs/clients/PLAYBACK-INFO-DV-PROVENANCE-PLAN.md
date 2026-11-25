# Playback info — Dolby Vision metadata and display target rows

**Status:** proposed — not started; awaiting owner go-ahead · **Written:**
2026-11-23 · **Owner:** Claude · **Source baseline:** `5980910`

Companion to [PLAYBACK-INFO-REDESIGN.md](PLAYBACK-INFO-REDESIGN.md) (the panel's
hierarchy and measurement rules) and
[HDR-DISPLAY-PEAK.md](HDR-DISPLAY-PEAK.md) (where a display peak comes from).
This plan adds two rows to Playback info so a viewer can see which Dolby
Vision metadata reached the picture and what display the stream was mapped
for. The idea comes from the
[CB1 comparison](../reviews/CB1-COMPARISON-2026-11-23.md#interface-added-2026-11-23).

Work milestone by milestone. The rows report; they decide nothing. If a step
seems to require changing a playback decision, a bucket rule or what the
server processes, stop and flag it.

## 1. Objective

Today a viewer watching a Dolby Vision title as enhanced HDR10 can see that
FEL, RPU and DYN are active, but not which metadata levels were used, nor the
display peak the stream was mapped for. The peak, its source and the bucket
are recorded by the server and shown only on the admin Activity page. A
viewer whose client reports no peak gets an unmapped stream and no hint why.

Two rows close that:

```
 Dolby Vision metadata        In the file         L1 · L2 · L5 · L6 · L8 · L9 · L11 (CM v4.0)
                              Applied by Cinema   L1 · L5 · L8
                              Left for the display  None; delivered as HDR10

 Display target               Reported peak       1000 nits · your setting
                              Mapped for          1000 nits
                              Title peak          1412 nits
```

## 2. What exists today (re-verify at build time)

| Fact | Where |
|---|---|
| Field list is generated from one contract file shared by web, Apple and Android | [`tests/playback/playback-info-fields.json`](../../tests/playback/playback-info-fields.json), embedded by [`scripts/player-contract-table`](../../scripts/player-contract-table) |
| One field has one label in every client and mode | [PLAYBACK-INFO-DIMENSIONS.md](PLAYBACK-INFO-DIMENSIONS.md) §7 |
| The server's completed-processing report reaches the player, fenced by generation | `DvEffectiveProcessingReport` in [`dv_processing.rs`](../../crates/plurx-core/src/transcode/dv_processing.rs); [API.md](../API.md) `effective_processing` |
| That report carries `applied_operations`, `creative_trims_applied`, `creative_trims_partial`, `fel_contributed` and the target nits | same struct |
| The helper already collects the set of metadata levels per frame | `metadata_levels` in `dv_processing.rs` |
| Reported peak, peak source and bucket are shown to admins only | `vod_observation.display_target` in [`activity-stream.js`](../../crates/plurxd/src/web/pages/activity-stream.js) |
| The title's L1 maximum is scanned and stored | `max_l1_pq` in [`scan/dv_title.rs`](../../crates/plurx-core/src/scan/dv_title.rs) |
| Per-shot L1 values are not stored | same file: only title totals |

## 3. Contract

Two new fields in the contract file, section NOW DECODING, modes `details`
and `debug`, format `list`:

| id | Label | Sub-rows, in order |
|---|---|---|
| `dv_metadata` | Dolby Vision metadata | In the file · Applied by Cinema · Left for the display |
| `display_target` | Display target | Reported peak · Mapped for · Title peak |

The server computes both and sends them; no client derives a level list from
`applied_operations`, because three clients deriving it three ways is how
labels drift.

**`dv_metadata`** — absent for a non-Dolby-Vision source.

- `in_file`: sorted level numbers plus the CM version, from the title scan.
- `applied`: levels the served picture consumed. Empty on direct play, copy
  and Profile 7 → 8.1 conversion, where Cinema applies nothing.
- `left_for_display`: levels still in the delivered stream for the display
  to apply. Empty when the delivery is HDR10 or SDR.
- Unknown is not empty: if the title scan is missing, `in_file` is
  "Unavailable", never a blank list.

**`display_target`** — present only when the delivery is a processed one.

- `reported_peak_nits` and `peak_source` (`platform` or `viewer`), or absent.
- `state`: `targeted` (with `mapped_nits`), `already_fits`, or
  `not_targeted` (with the server's reason sentence).
- `title_peak_nits`, from the scanned L1 maximum.

Both ride the existing generation-fenced report and clear with it on seek,
fallback or replacement. No new endpoint and no extra poll.

## 4. Non-goals

- **No change to any decision.** These rows are reporters, as the badge work
  already requires.
- **No display introspection.** The rows show what the client reported, not
  a measurement of the panel, and say so.
- **No "This shot" row in this plan.** Live per-shot brightness needs a
  per-shot L1 timeline that is not stored today. It is §5.5, a separate
  decision.
- **No boxed cards.** Rows use the panel's existing hairline rows.
- **No status by colour alone.** "Not targeted" is words.

## 5. Milestones

### 5.1 Contract and server report

Add both fields to the contract file and regenerate the embedded tables.
Extend the processing report with the two structures in §3, with the
operations-to-levels mapping defined once on the server. Update
[API.md](../API.md).

**Acceptance:** contract-table check passes; Rust tests cover direct play
(nothing applied, everything left), conversion to 8.1, enhanced HDR10 with
and without a peak, the already-fits case and a missing title scan.

### 5.2 Web

Render both rows in details and debug modes from the shared renderer.

**Acceptance:** `tests/web` cases for the three states in the
[mock-up's](../reviews/CB1-COMPARISON-2026-11-23.md#interface-added-2026-11-23)
description: targeted, not targeted, already fits; rows clear on a seek that
drops the report.

### 5.3 Apple and Android

Same rows from the same contract, in each client's existing panel.

**Acceptance:** one new identity case per client proving the labels and
sub-row order equal the contract; no client-specific label.

### 5.4 Linux player

Add both rows to the mpv overlay's info list, and group that list under
Input · Processing · Output headings to match the other clients.

**Acceptance:** Lua scenarios for the three states and for the headings.

### 5.5 Decision: live per-shot values

Decide whether to store and deliver a per-shot L1 timeline so a "This shot"
row can show the current shot's min / average / max. Record the storage and
wire cost before building anything.

**Acceptance:** a recorded yes or no with the measured cost.

## 6. Risks

- **The applied-levels mapping can lie if it is guessed.** It must come from
  what the helper actually consumed per window, with partial application
  reported as partial, as `creative_trims_partial` already does.
- **Contract churn across three clients.** 5.1 lands the contract alone so
  each client change is small and independent.
