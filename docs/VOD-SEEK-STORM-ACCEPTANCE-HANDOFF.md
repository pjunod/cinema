# Restore VOD seek-storm acceptance

**Status:** disabled in CI after a reproducible acceptance failure on
2026-08-26; the case and all of its assertions remain in the VOD suite.

## Paste this into the next implementation session

```text
Restore plurx's VOD seek-storm browser acceptance and re-enable it in CI.

Start from current main and read, in order:

- docs/VOD-PRESENTATION-PLAN.md;
- docs/VOD-CUTOVER.md;
- this handoff;
- tests/playback/cases.json (the VOD suite and seek-storm thresholds);
- scripts/playback-lab (performOperation and scoreCase);
- crates/plurxd/src/vodserve.rs (serving-authority validation and fencing).

Observed failure:

- GitHub Actions run 32929741906, job 98059637890;
- steady playback passed and suspend/resume passed;
- all 20 native VOD seeks landed without creating a replacement session;
- seek-storm failed with 9 visible frame hitches against a maximum of 2;
- the browser reported a bufferStalledError with about 0.1 seconds of runway;
- the server reported that serving authority expired and the mutable media was
  self-fenced, plus transient replicated-authority read retries.

Do not delete the seek-storm case, loosen its hitch threshold merely to make CI
green, bypass serving-authority fencing, or restore the retired live producer.
Reproduce the failure on a self-hosted runner, determine whether authority
renewal, seek scheduling, or the player's buffer strategy causes the runway to
collapse, and fix the production or harness root cause. Keep the steady and
suspend/resume cases green throughout.

When the case is stable, remove `--exclude-case seek-storm` from the VOD browser
acceptance command in .github/workflows/ci.yml.

Done means:

1. The seek-storm case passes three consecutive self-hosted CI-equivalent runs
   with all 20 seeks landed, no replacement session, and at most 2 hitches.
2. A focused regression test covers the identified authority, scheduling, or
   buffering defect without weakening self-fencing.
3. The full three-case VOD suite is restored as a CI release gate.
4. The full PR checks and the next main workflow are green.
5. Any changed runtime behavior and retained evidence are documented.
```

## Current CI boundary

CI currently selects only the VOD `suspend-resume` case. This keeps the
disabled seek-storm case executable locally while retaining one browser
acceptance gate. Steady playback has its own restoration prompt in
`docs/VOD-STEADY-ACCEPTANCE-HANDOFF.md`. The pure seek-storm scoring and
manifest contracts continue to run in `tests/playback/network-shaping.test.js`.
