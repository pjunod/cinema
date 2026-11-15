# Apple per-build release notes

One file per Apple change. This directory exists because the narrative used to
live in three shared documents at a shared insertion point, so any two
concurrent Apple branches conflicted on all three and re-conflicted every time
`main` moved. Issue #509 records the decision and the rejected alternatives;
[`history-through-build-78.md`](history-through-build-78.md) holds everything
those documents said through build 78, verbatim.

[Keep the offload banner current on Apple devices](1-offload-client-refresh.md) records the follow-up client build.

## Add a fragment

Name the file after the **issue**, never the build:

```
docs/apple-builds/<issue>-<short-slug>.md
```

Issue-keyed names are the whole point. Two concurrent Apple changes write two
different paths, so the narrative cannot collide.

```markdown
# Say when a Skip Credits button is an estimate

Issue: #351

Skip Credits now says when its end-credits boundary is estimated rather than
detected, so a viewer can tell a confident jump from a guess.
```

`Issue:` is required and appears once. A fragment carries no build number: a
TestFlight build is numbered when it is released, from the commit it is built
from ([SHARED-BOOKKEEPING.md](../ci/SHARED-BOOKKEEPING.md#store-build-numbers)),
so no branch claims one and there is nothing to re-claim when `main` moves.
Fragments written before 2026-11-15 keep the `Build:` line that records the
build they shipped in.

## What still fails the gate

`tests/operations/test_apple_build_claims.py` holds the fragment rules,
refuses per-build narrative in the Apple, Android and parity status
blockquotes, and merges two branches cut from one base in either order to
prove they do not collide. `tests/operations/test_mobile_build_claims.py`
refuses a status line or an unmarked `STATUS.html` sentence that states a
build number, and `validation/mobile_versions.py` refuses a tree whose
manifests carry anything but the development build.

Settings navigation: [Keep category input in its remote adapter](1355-settings-category-adapter.md)
records the behavior-preserving input-fence repair.

## Apple TV playback

[Return playback to its title after sleep](1347-apple-tv-sleep-wake.md) records
the tvOS background teardown, position handoff and pending physical acceptance.

[Keep the timeline moving while a direction is held](1348-apple-tv-held-seek.md)
records continuous directional previews and release/cancellation behavior.

## Notes moved from the docs index

iPhone Live TV guide: [Restore the station-logo time grid](1273-iphone-guide-grid.md).

Apple TV timeline Select: [Commit the previewed seek position](1264-tvos-timeline-select.md).

These lines used to sit in `docs/README.md`; they point at fragments in this folder.

Apple build 160: [Native Live TV and shelf layouts](320-native-layout-followup.md).

Native Home restoration: [Original Apple Home screens](334-original-home.md) — **open**.

Apple Home clipping: [Featured movie content bounds](570-home-hero-clipping.md).

Apple playback controls: [10 and 30 second seeking](689-playback-seek-controls.md).

Apple seek icons: [Directional arrows for seek distances](694-seek-arrow-icons.md).

Apple playback buffer: [944-playback-buffer-limit.md](944-playback-buffer-limit.md).

Apple temporary serving fence: [962-temporary-serving-fence-control.md](962-temporary-serving-fence-control.md).

Apple display wake: [818-display-wake.md](818-display-wake.md) — playback owns the display idle timer.

Apple build 222: [Verified Dolby Vision processing reports](968-dv-processing-reports.md).

Apple build 137: [Library channel startup and contrast](245-library-channel-start-and-contrast.md).

Apple build 138: [Library channel playback decision](245-library-channel-playback-decision.md).

Apple build 141: [Library channel buffering demand](245-library-channel-buffering-demand.md).

Apple build 143: [Playback lifecycle recovery and prepared handoff](257-playback-lifecycle.md).

Apple build 147: [The playback surface contract](278-playback-surface-contract.md).

Apple build 151: [Playback surface reach](278-playback-surface-reach.md).

Apple build 152: [Banner actions](278-banner-actions.md).

Apple build 153: [Live TV stall and fullscreen surface](300-live-tv-surface.md).

Previous: [Live TV playback method](245-live-tv-playback-method.md).

| Note | Answers | |
|---|---|---|
| [Subtitle text translation and shared captions](1292-subtitle-completion.md) | What does Apple build 277 add to subtitle translation and shared playback? | open |
| [Display target session source contract follow-up](1216-display-target-session-contract.md) | What model-boundary contract coverage accompanies the display-target session work? | built |
| [SDR display target contract follow-up](1216-sdr-display-target-contract.md) | What typed SDR processing contracts follow the display-target session work? | built |
| [Ratings, watchlist and favorites](1205-ratings-watchlist-favorites.md) | What do the ratings and list controls add to iOS and tvOS? | built |
| [Server label names the machine](1204-server-hostname-label.md) | What does Settings call the server you are signed in to? | built |
| [Remote diagnostics Apple wire](remote-diagnostics-wire.md) | What is added to the Apple report format contract? | built |
| [Remote diagnostics Apple capture](remote-diagnostics-capture.md) | What changes in native private report capture, share, send and TV approval? | open |
| [remote-diagnostics-qualification.md](remote-diagnostics-qualification.md) | Qualifies offline phone ZIP export, native TV local downloads and same-incident relaunch recovery. | built |

Apple growing playlist attachment: [preparation and failover correction](914-growing-playlist-attachment.md) — **built; physical acceptance open**.

- [Apple TV focus responsiveness](1130-apple-tv-focus-responsiveness.md) —
  input-update isolation, regression evidence and remaining device acceptance.

[External Cinema invitation](1323-external-sharing-invitation.md) — combined network consent and automatic import continuation.
Credential sign-in: [Keep the report action off credential sign-in screens](1321-sign-in-report-button.md).

[Progressive caption recovery](1329-progressive-caption-recovery.md) — stopped-operation discovery, explicit Resume and current-range failures.

[Captured session release](1331-captured-session-release.md) — Stop releases its attached HLS session through the configured owner.
