# Marker-destination prewarm — the consumer its metric is waiting for

**Status:** ready to build · **Executes:** the prewarm bullet of
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) §13.8,
split out of [M7-REMAINDER-HANDOFF.md](M7-REMAINDER-HANDOFF.md) §7 per Paul,
2026-09-01 · **Written:** 2026-09-01 · **Baseline:** `main` at `e1876cce`
(`v0.3.0`)

## 1. Orientation

*"Use the control snapshot to prewarm marker destinations without seeking the
client."* This is the one §13.8 bullet that is separable from the subtitle
and seek work, so it lives in its own plan. It depends on nothing in
[M7-REMAINDER-HANDOFF.md](M7-REMAINDER-HANDOFF.md) to *build*, but two of
that plan's pieces make this one better when they exist: M3's settled-target
latch (prewarm work triggered for a seek the client has already abandoned is
exactly the obsolete production that latch cancels) and M2's window
extraction (the subtitle half of warming a destination). Build this after
M3 lands, or accept that its production joins the latch retroactively.

Line numbers are from `e1876cce`; re-verify against the tree at build time.

## 2. What exists today — verified 2026-09-01

The feature's instrumentation shipped without the feature. #700 landed the
annotation index (exact marker end times with provenance/confidence, manual
overrides that win permanently) and the skip instrumentation on all three
clients — but every prewarm callsite emits a hard-coded `"miss"`
(`crates/plurxd/src/web/index.html`, `PlayerController.swift`,
`PlayerScreen.kt`), no `"hit"` producer exists anywhere in the product, and
`plurx_playback_marker_prewarm_hit_ratio`
([`telemetry.rs:239–248`](../crates/plurxd/src/telemetry.rs) — hits over
hits+misses from `plurx_playback_marker_prewarm_total`) is therefore pinned
at `0.000000` permanently. On a dashboard that reads as a broken feature
rather than an absent one; the pinned zero is at least honest, and this plan
exists to make it move for the right reason.

What the actor already holds, per control exchange
([`playback_control.rs`](../crates/plurxd/src/playback_control.rs),
`PlaybackDemandSnapshot`): `position_ms`, `playback_rate`,
`buffered_through_ms`, the selection. What the producer already reports:
`produced_through_ms`. What the annotation index holds: the stored exact end
time of each intro/credits marker. Every input the decision needs is in one
place, in sequence order — nothing is reconstructed from media fetches.

## 3. The design

Actor-side, on the ordinary control exchange, no client action emitted:

- When `position_ms` (at the current `playback_rate`) approaches a marker
  whose stored exact end time lies beyond `produced_through_ms`, ask the
  producer for that destination through the same bounded ahead-window
  machinery playback already uses — aimed at the skip target instead of the
  playhead. The client is never seeked and never told; if it skips, the
  destination is already produced.
- The subtitle track selected at the time prewarms alongside, through
  M2's window extraction once it exists (one window at the destination).
- Prewarm bookkeeping records, per playback, which destination ranges were
  produced *because of prewarm* and when. This ledger is what the metric's
  `"hit"` is judged against — see §4.
- Prewarm production is subordinate: it must never starve the playhead's own
  ahead window, and (once M3 exists) it keys into the settled-target latch
  so an abandoned seek cancels it like any other obsolete production.

Direct play has nothing server-side to warm. Its callsites keep emitting
`"miss"`, and that is correct — the ratio then reports exactly the share of
skips the server could and did help.

## 4. The metric contract — learned the expensive way, not negotiable

- **A `"hit"` is emitted only when a skip lands on a destination this
  prewarm actually produced** — the prewarmed range covered the landing
  point at skip time, verified against §3's own bookkeeping.
- **Never redefine hit as "destination happened to be in the client's
  ordinary forward buffer."** That was attempted and rejected in review
  once already. On direct play the browser buffers minutes ahead, so the
  ratio degenerates into a proxy for the transport mix — already available
  from the transport labels — and an unbuilt feature reads as built and
  working, which costs an operator more than the pinned zero does.
- A measurement wanted before or beside the feature gets **its own name or
  its own label**, never a redefinition of
  `plurx_playback_marker_prewarm_hit_ratio`.

## 5. Non-goals

- **No detectors.** Where an intro *is* stays gated on a labelled corpus
  and a false-positive-weighted evaluation, separately.
- **No recovery opinions.** Recovery authority is the server's
  `ControlAction`; prewarm proposes nothing and cancels nothing outside its
  own production.
- **No client-visible seeks or actions.** The bullet's own wording:
  *without seeking the client*. The clients' only change is emitting
  `"hit"` when the server tells them the landing was prewarmed — and if the
  design can carry hit attribution entirely server-side, the clients change
  nothing at all (preferred: no build-number bumps).
- **Subtitle prewarm never touches the video pointer** — the same
  video-independence criterion that governs the M7 remainder.

## 6. Milestone and acceptance

One milestone; the feature is small once its inputs are honest.

**Acceptance:** on a fixture with stored markers, a skip whose destination
was prewarmed emits `hit`; one whose destination was not emits `miss`;
direct play emits `miss`; prewarm production never runs for a target the
latch has settled away from (once M3 exists); and
`plurx_playback_marker_prewarm_hit_ratio` moves off `0.000000` for the
first time for the right reason.

```bash
cargo test -p plurxd prewarm
curl -s localhost:8080/metrics | grep marker_prewarm
```

Repo mechanics (validation points, history-check, build bumps if clients
change) are as [M7-REMAINDER-HANDOFF.md](M7-REMAINDER-HANDOFF.md) §10
states them.
