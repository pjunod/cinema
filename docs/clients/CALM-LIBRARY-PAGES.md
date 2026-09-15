# Library pages — scan your library and inspect one file

**Status:** built · PR #330 awaiting final lane · **Updated:** 2026-09-15

Companion to [CLIENTS.md](../CLIENTS.md). This revision implements the approved
Home and item-page proposals across the web, Apple and Android clients.

## Home starts with the library

Recently added comes first and excludes libraries whose type is `recordings`.
Continue watching shows three compact entries, with the remainder behind More
in progress. Recently recorded stays separate. A generic Next up shelf and
featured continuation hero no longer appear. Series continuation stays attached
to the named series or season page.

The web waits for the library roster before classifying recent entries; an
unknown library does not get guessed to be a movie library. Web library previews
and upcoming releases remain under disclosures. Native library destinations
remain available from their existing navigation.

## The item header describes the selected file

Resolution, dynamic range, video codec and container are the four colored
header badges. Other facts are text. A version selector changes the displayed
file and the primary playback action together. Track selections belong to that
file and this playback; they do not change account defaults.

Media stays open with video, audio, subtitles, preparation and diagnostics.
Audio and subtitle summaries remain visible when the lists are collapsed.
Each expanded list supports searching and initially displays six matching
tracks; Show all expands the result. Off and Use server default retain their
existing selection semantics. The complete filename is available below.

English availability is based on language tags. Untagged streams and unprobed
files say availability is not confirmed; absence of a tag is not proof of a
missing English track. Preparation comes from the server's VOD index status,
never from a guessed successful playback. Diagnostics report metadata
availability without inventing a health verdict or a last-checked timestamp.

## Scope and deliberate choices

This is one cohesive revision, delivered as one main-bound PR. There are no
rollout flags or new enablement conditions. Existing Developer enable controls
and their advisory readiness information remain in place. Live TV, player
statistics, ingestion and recording scheduling are outside this revision.

Existing native playback, download and remote input controls are reused.
Native TV typography and controls follow the platform, with the same hierarchy
as the web proposal. Illustration artwork in the proposal is not shipped.

## Evidence and remaining work

Production web pages were rendered at 1280 px and 390 px with sample data in
all three themes. No JavaScript errors or phone overflow were observed. File
changes updated all four header badges; expanded track lists showed six rows.
The implemented Apple DetailView was captured in disposable iPhone, iPad and
Apple TV simulators using an isolated sample-data harness. Android visuals,
physical-device playback and full remote-focus acceptance remain unverified.

Local iOS/tvOS compilation (`make apple-build`) and Android application
compilation (`./gradlew :app:assembleDebug`) passed after review. Local Xcode
is 27.0; the final Apple lane uses the pinned Xcode 26.6 runner. Rust 1.97.1
`cargo check -p plurxd --all-targets --locked`, workspace Clippy with denied
warnings and formatting passed. Focused regressions:

- `node tests/web/calm-library.test.js`
- `cargo test -p plurx-core global_recent_additions_exclude_recordings_before_limiting --locked`
- `cargo test -p plurxd --bin plurxd original_download_supports_ranges_without_registering_playback --locked`

The download regression initially found its legacy sample file shorter than
its recorded length. Matching the fixture to the probe made the ranged
response pass without weakening the production file-size check. Retained
native expectations reflect the removed hero and collapsed track disclosures.

One adversarial review completed; all five P2 findings are addressed. PR #330
runs one final fast lane after these records are complete. This document does
not claim a merge, deployment, publication or physical-device installation.

## Review findings addressed

- Android's file summary no longer enumerates every audio track outside the
  collapsed list.
- Both catalogue stores exclude recording libraries before the global Recently
  added limit. An explicitly scoped recordings library still returns its items.
- Apple and Android recording rows open the named recording details.
- Web version changes recompute subtitle cost notices and reject responses for
  detached notices; track choices stay attached to their file.
- Web home videos and recordings use the shared item composition, including the
  existing home-video metadata editor in More actions.

The original download action uses a dedicated authenticated, range-capable
attachment endpoint. Downloading bytes does not create a watching session.
