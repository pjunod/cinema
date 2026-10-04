# Native Auto-quality policy runners — source only

**Status:** 2026-09-30 — draft A05 M2; no native playback enablement or device acceptance.

Build: 206
Issue: #634

The Apple and Android clients now contain pure ports of the current browser
`PlaybackPolicy.decideRung` with standard XCTest and JVM JUnit runners of the
single shared `tests/playback/auto-quality-policy.json`. The same 32 cases and
20 supplied defaults drive all three languages; the unresolved switch-budget
proposal remains a recorded `web_current` disagreement, not a new algorithm.
Adding a deliberately wrong case to the same JSON failed all three at the
named height assertion; the restored fixture passed again.

No existing player controller calls these ports. No timer, bandwidth adapter,
setting, frame instrumentation or reopen reason is changed. Apple 202 and
Android 140 are changed-source build bookkeeping with unchanged version 0.3.0,
not signed releases, installed products, D3 measurements or physical acceptance.
Design §3.6 explicitly permits these pure runners before D3; adapters remain
subject to the design's own measured-baseline dependency.

Focused iOS/tvOS XCTest, Android JVM JUnit and existing web policy results are
recorded in [PR #634](http://192.168.4.7:3000/noirr/plurx/pulls/634). The
[A05 build plan](../clients/NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md) retains the
execution history and remaining native work.
