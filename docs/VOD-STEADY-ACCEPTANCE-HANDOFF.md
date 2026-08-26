# Restore VOD steady-play acceptance

**Status:** disabled in CI after a reproducible acceptance failure on
2026-08-26; the case and all of its assertions remain in the VOD suite.

## Paste this into the next implementation session

```text
Restore plurx's VOD steady-play browser acceptance and re-enable it in CI.

Start from current main and read, in order:

- docs/VOD-PRESENTATION-PLAN.md;
- docs/VOD-CUTOVER.md;
- this handoff;
- tests/playback/cases.json (the VOD suite and steady thresholds);
- scripts/playback-lab (runSuite, performOperation, and scoreCase);
- crates/plurxd/src/vodserve.rs (serving-authority validation and fencing).

Observed failure:

- GitHub Actions run 32943477931, attempt 2, job 98105071882;
- the steady case failed with 4 visible frame hitches against a maximum of 2;
- hls.js reported a cold-start bufferStalledError at 0.0 seconds;
- the server later measured presentation at 0.165x with 21.3 seconds buffered;
- serving authority expired and mutable media self-fenced during the case;
- suspend/resume passed in the same run: 201 ms TTFF, 0.991x clock, 0 hitches,
  and 0 stalls.

Do not delete the steady case, loosen its hitch threshold merely to make CI
green, bypass serving-authority fencing, or restore the retired live producer.
Reproduce the failure on a self-hosted runner and determine whether cold-start
buffering, browser scoring, VOD materialization, or authority renewal causes
the initial stalls and slow presentation. Keep suspend/resume green throughout.

Run the focused case with:

scripts/playback-lab run --suite vod --case steady --browser chrome \
  --chrome "$chrome" --json target/playback-lab/acceptance/vod-steady.json

When the case is stable, change the VOD browser acceptance command in
.github/workflows/ci.yml from `--case suspend-resume` to
`--exclude-case seek-storm`. That restores steady plus suspend/resume while the
separate seek-storm investigation remains explicit.

Done means:

1. The steady case passes three consecutive self-hosted CI-equivalent runs with
   at most 2 hitches, no stalls, and a clock rate of at least 0.90x.
2. A focused regression test covers the identified buffering, materialization,
   scoring, or authority defect without weakening self-fencing.
3. Steady and suspend/resume are restored together as CI release gates; the
   seek-storm exclusion remains until its own handoff is complete.
4. The full PR checks and the next main workflow are green.
5. Any changed runtime behavior and retained evidence are documented.
```

## Current CI boundary

CI selects only `--case suspend-resume`. The steady case remains executable
locally with `--case steady`; its manifest and scoring contracts are unchanged.
Seek-storm and bandwidth-cliff recovery remain tracked by their own handoffs.
