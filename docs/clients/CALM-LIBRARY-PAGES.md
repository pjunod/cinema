# Library pages — scan your library and inspect one file

**Status:** built in PR #330; web Home restoration in PR #331 · **Updated:** 2026-09-15

Companion to [CLIENTS.md](../CLIENTS.md). This revision implements the approved
Home and item-page proposals across the web, Apple and Android clients.

## Web Home returns to its previous layouts

On September 15, 2026, the user requested restoration of the Classic, Catalog
and Theater home pages from before the usability redesign (PR #317). Their
original Continue watching and Next up poster rails, Coming soon shelf and
library previews are restored. The later oversized continuation cards,
Next episode panel and Home-only recording section are removed. Theater keeps its full-width featured title above the shelves;
Classic uses library grids and Catalog uses library rails.

The compact web Home composition and its unused styles are removed. The
server still excludes recording libraries from global Recently added before
the limit. Item pages and playback information retain the PR #330 design.
This restoration targets the three web layouts; native Home is unchanged.

Apple and Android Home still lead with Recently added, with three compact
Continue watching entries, a disclosure for the remainder, and a separate
Recently recorded section. Their generic Next up shelf remains removed.

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
as the web proposal. Illustration artwork in the proposal is not shipped. Theater uses the existing
backdrop and resume behavior; without an in-progress video it can feature a
recent library video, excluding recording libraries.

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

PR #330 merged at `23d8e3a0` after one adversarial review and all five P2
findings were addressed. Policy, release versions, Rust, web, Apple and
Android passed. The user explicitly waived Windows and the dependent
promotion gate for that PR. No deployment, publication or physical-device
installation was performed. The subsequent web Home restoration receives
its own review and validation. Its single adversarial review found no
actionable issues; the final Home renderers use the pre-PR #317 poster-rail composition.

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
