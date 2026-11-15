# Startup and tone-map: fewer create-path reads, a tone-map census, lab arms

**Branch `effort/startup-tone-map`.** Executes [STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md](../docs/streaming/STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md)
rev 6.

- **Part B is built.** The create read-budget line had its authority and local labels swapped; that is corrected, and the line now carries `store_ms` and a per-phase split. Seven read reductions follow, none of which moves a read across a fence or changes its consistency.
  - On a two-voter hiqlite test cluster, store time is nearly all of the handler's time on every request shape.
  - Two regressions pin the source-replacement interval.
  - The candidate-route rolling fallback re-validates its binding before starting. Without that, a carried snapshot could start the producer from a source that had already been replaced.
- **The tone-map census (C-M0) and the single Dolby tonemapx builder (C-M1) are built.** The census is admin-started from System → HDR tone-map, yields to playback, can be stopped, and stores its results node-locally. Selection and filter strings are unchanged.
- **The A-M0 lab arms are built.** The lab names the bootstrap value B only from runs of at least 30 minutes.
- **Owed, all by the GPT session:**
  - the lab runs (§9.3), which pick B;
  - the census on every node (§9.2), which goes to Paul for R2;
  - the two-build B-M4 measurement (§9.1).
- **Waiting on those:** Part A's A-M1/A-M2 waits on B, and C-M2/C-M3 wait on R2.
