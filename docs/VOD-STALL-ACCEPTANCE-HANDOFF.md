# Restore VOD bandwidth-recovery acceptance

**Status:** blocked on D6 device measurement; the standalone harness remains
available, but its CI invocation was disabled on 2026-08-26.

## Paste this into the next implementation session

```text
Restore plurx's VOD stall-recovery browser acceptance and re-enable it in CI.

Start from current main and read, in order:

- docs/VOD-PRESENTATION-PLAN.md, especially decision D6;
- docs/VOD-M4-HANDOFF.md, especially "Deliberate boundary";
- docs/VOD-CUTOVER.md;
- this handoff;
- tests/playback/cases.json (the stall-recovery suite);
- scripts/playback-lab (startServer and runSuite);
- crates/plurxd/src/vodserve.rs (the vod_transcode_unavailable gate).

Observed failure:

- GitHub Actions run 32918531993, job 98032989197;
- the three film-addressed VOD cases passed 3/3;
- the shaped 8mbps-to-1.5mbps case timed out before its first frame;
- the server returned vod_transcode_unavailable because Auto requested a
  transcoded rung while VOD transcode serving is gated on D6.

Why the old green result is not sufficient: the stall suite passed on
50380267 before the VOD-only cutover. PR #573 made every segmented HLS session
VOD-only, so the test can no longer fall through to the retired live producer.

Do not remove the typed safety refusal, restore legacy live HLS, fake a device
measurement, or weaken the recovery assertions. Complete the D6 evidence and
implementation needed for an honest VOD transcode rung, or redesign the case
only if it still proves an actual bandwidth-driven recovery supported by the
current VOD contract. Preserve the three existing VOD acceptance cases.

When the product can serve the lower rung, restore this command to the
film-addressed VOD browser acceptance job in .github/workflows/ci.yml:

scripts/playback-lab run --suite stall-recovery --browser chrome \
  --chrome "$chrome" --network-profile 8mbps-to-1.5mbps \
  --json target/playback-lab/acceptance/stall-recovery.json

Done means:

1. AVPlayer and Media3 evidence satisfies the D6 device-measurement contract,
   with retained artifacts and exact build/device versions.
2. VOD transcode-rung session creation and immutable serving are covered by
   focused Rust tests and return typed failures when ineligible.
3. The standalone stall command passes three consecutive runs without using
   the legacy live producer or bypassing network shaping.
4. The CI invocation above is restored, and the full PR checks plus the next
   main workflow are green.
5. VOD, playback, status, and operator documentation describe the landed
   behavior and the retained evidence.
```

## Current CI boundary

CI still runs `scripts/playback-lab run --suite vod`, so steady playback,
twenty non-linear seeks, and suspend/resume remain release gates. Only the
bandwidth-cliff case is disabled. Its pure shaping, scoring, lifecycle, and
artifact contracts continue to run through
`node tests/playback/network-shaping.test.js` on every commit.
