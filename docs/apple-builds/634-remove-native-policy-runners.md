# Remove the unused native Auto-quality policy runners

**Status:** built — 2026-10-04, source only; no signed release, install or device check.

Build: 206
Issue: #634

The pure Swift and Kotlin ports of `PlaybackPolicy.decideRung` that #634 added
([Native policy runners](634-native-policy-runners.md)) are deleted, with their
XCTest and JVM runners and the Rust reducer `decide_auto_transition` /
`auto_prepared_commit_allowed` in `plurx-core`'s `playback/candidate.rs`. No
player, timer or adapter ever called any of them; the 2026-10-04 architecture
relevance pass (row A-05) found them orphaned and the owner's ruling was to
remove rather than wire them.

Nothing a viewer can observe changes. The shipping native controllers keep the
thresholds they already had. `tests/playback/auto-quality-policy.json` stays:
the web runner in `tests/playback/web-policy.test.js` still reads it, and a
future native adapter that ports the policy again has to read it too rather
than restate its constants.

The deletion is a release input on both clients, so the Apple build and the
Android versionCode move; neither is a signed or installed release.
