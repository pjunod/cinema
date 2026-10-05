# Native Auto-quality policy runners — source only

**Status:** open — A05 M2, on `main` since 2026-10-04 (#793) in Apple build
206; no native playback enablement or device acceptance; the ports are
deleted in the 2026-10-04 close-out PR.

Build: 208
Issue: #634

The Apple and Android clients now contain pure ports of the current browser
`PlaybackPolicy.decideRung` with standard XCTest and JVM JUnit runners of the
single shared `tests/playback/auto-quality-policy.json`. The same 32 cases and
20 supplied defaults drive all three languages; the unresolved switch-budget
proposal remains a recorded `web_current` disagreement, not a new algorithm.
Adding a deliberately wrong case to the same JSON failed all three at the
named height assertion; the restored fixture passed again.

No existing player controller calls these ports. No timer, bandwidth adapter,
setting, frame instrumentation or reopen reason is changed. The change was
first claimed as Apple 202 / Android 140. It reached `main` with the
architecture effort (#793, 2026-10-04), and `main` has held Apple build 206
and Android versionCode 144 since, at version 0.3.0; build 206 is the
effort's counter and carries every Apple change on `main` at that point, not
this change alone. It is not a signed release, an installed product, a D3
measurement or physical acceptance. This change's own
`AutoQualityPolicyTests.testSharedAutoQualityFixture` failed in the
2026-10-02 simulator run of the effort branch at `6f6466ebc`
([APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md §6.1](../clients/APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md));
no run on `main` is recorded. The ports are deleted in the 2026-10-04
close-out PR, since nothing in production calls them.

Design §3.6 explicitly permits these pure runners before D3; adapters remain
subject to the design's own measured-baseline dependency.

Focused iOS/tvOS XCTest, Android JVM JUnit and existing web policy results are
recorded in [PR #634](http://forge.lan:3000/noirr/plurx/pulls/634). The
[A05 build plan](../clients/NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md) retains the
execution history and remaining native work.
