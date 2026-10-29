# Apple per-build release notes

One file per Apple change. This directory exists because the narrative used to
live in three shared documents at a shared insertion point, so any two
concurrent Apple branches conflicted on all three and re-conflicted every time
`main` moved. Issue #509 records the decision and the rejected alternatives;
[`history-through-build-78.md`](history-through-build-78.md) holds everything
those documents said through build 78, verbatim.

## Add a fragment

Name the file after the **issue**, never the build:

```
docs/apple-builds/<issue>-<short-slug>.md
```

Issue-keyed names are the whole point. Two concurrent Apple changes write two
different paths, so the narrative cannot collide, and neither branch has to
guess which build number it will end up shipping as.

```markdown
# Say when a Skip Credits button is an estimate

Build: 79
Issue: #351

Skip Credits now says when its end-credits boundary is estimated rather than
detected, so a viewer can tell a confident jump from a guess.
```

`Build:` and `Issue:` are required and each appears once. `Build:` is the only
number in the fragment that the tooling rewrites, and it is rewritten for you.

## Claim or re-claim the build number

```
make apple-build-bump
```

That reads `clients/apple/project.yml` and the merge target, picks the next
number above both, and rewrites every place the number is allowed to appear:
`project.yml`, the anchored `> Status:` lines in
[`clients/apple/README.md`](../../clients/apple/README.md) and
[`APPLE-CLIENT-PARITY.md`](../clients/APPLE-CLIENT-PARITY.md), the viewers tile and the
`👤 Paul` TestFlight item in [`STATUS.html`](../STATUS.html), and the `Build:`
line of the fragments this branch adds.

Run it again after `main` moves under an open branch, **in that order**: merge
`main` in first, then claim.

```
git merge origin/main      # conflict-free: both sides claimed the same number
make apple-build-bump      # now claims one above what main actually holds
```

The counter has to beat the merge target, not the commit the branch was cut
from. Claiming before the sync puts the branch two above the base while `main`
holds one above it, on the same line — an ordinary same-line conflict, and the
one thing the tool cannot merge for you. Syncing first costs a merge Git
resolves by itself and leaves exactly one mechanical commit behind.

Claiming is idempotent. A branch already above the merge target keeps the number
it holds, so running the tool again when nothing moved changes nothing and
prints so.

## What still fails the gate

`tests/operations/test_mobile_build_claims.py` and `validation/mobile_versions.py`
are unchanged. The number still has to agree across `project.yml`, both
`> Status:` lines, and every unmarked `Apple build <n>` mention in `STATUS.html`,
and it still has to increase whenever Apple release inputs change.
`tests/operations/test_apple_build_claims.py` adds the fragment rules, pins the
generated form of every surface, refuses to let per-build narrative move back
into either status blockquote, and merges two branches cut from one base in
sequence to prove they no longer collide.

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
| [Captured session release](1331-captured-session-release.md) | How does Stop release its attached HLS session through the configured owner? | built |
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
