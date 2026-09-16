# Library pages — scan your library and inspect one file

**Status:** built in PR #330; web Home restored in PR #331; native restoration in PR #334; web item page restored · **Updated:** 2026-09-16

Companion to [CLIENTS.md](../CLIENTS.md). This revision implements the approved
Home and item-page proposals across the web, Apple and Android clients.

## Home returns to its previous layouts on every device

On September 15, 2026, the user requested restoration of the Classic, Catalog
and Theater home pages from before the usability redesign (PR #317). Their
original Continue watching and Next up poster rails, Coming soon shelf and
library previews are restored. The later oversized continuation cards,
Next episode panel and Home-only recording section are removed. Theater keeps its full-width featured title above the shelves;
Classic uses library grids and Catalog uses library rails.

The compact web Home composition and its unused styles are removed. The
server still excludes recording libraries from global Recently added before
the limit. Item pages and playback information retain the PR #330 design.
The follow-up restores the original native Home composition too. iPhone and
iPad regain their featured title, landscape Continue Watching and Next Up
shelves, Recently Added and Coming Soon. Apple TV regains its original media
shelves while retaining the later card-height and text-clipping repairs.
Android phones regain the compact featured continuation; tablets and
Android TV/Google TV regain their original shelves, library grouping and
D-pad focus chain. The compact three-entry lists and Home-only recording
sections are removed. Recording activity remains available in Live TV and
Recordings, including the Apple tab's attention count.

Home navigation and rendering are the scope of the restoration. Item pages,
track selection, playback information, Live TV and shared card repairs remain
intact. Existing native Home regression expectations are restored with their
original layouts. The one adversarial review found an Android TV focus race
in the historical source: its pending request could be cancelled by setting
claimed state too early. Home now marks the request claimed only after the
focus helper reports success. The focused Compose regression is compiled;
device execution is not claimed. Local iOS, tvOS and Android compilation
passed. The final lane runs once after the review finding is addressed;
no broad native unit suite is added.

## The web item page returns to its original layout

On September 16, 2026, the user compared renders of the web item page before
PR #317, after PR #317 and at current main, and chose the page from before
PR #317. Movies, series, seasons and episodes render through each layout's own
item body again: the poster panel, the single row of badge chips, the open
facts panel with its audio and subtitle chips, the pre-play pickers and the
Content analysis box. The shared "viewing" body that PR #317 introduced and PR
#330 recomposed — the version selector, the four coloured header badges, the
English-availability line, the Download button, the collapsible searchable
track lists and the series "Next episode" panel — is removed from the web
client along with its styles. The `/files/{id}/download` route and the
server-side fields PR #330 added stay; nothing on the web client calls them.
The native item pages are not part of this restoration and keep the PR #330
design described below.

## The item header describes the selected file (native clients)

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

- `node tests/web/calm-library.test.js` — its item-page checks now pin the original per-layout item bodies instead of the removed track summaries
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
Production web captures at 1280 px and 390 px cover all three restored layouts
with sample data; no script errors or horizontal overflow were observed.

## Review findings addressed

- Android's file summary no longer enumerates every audio track outside the
  collapsed list.
- Both catalogue stores exclude recording libraries before the global Recently
  added limit. An explicitly scoped recordings library still returns its items.
- Apple and Android recording rows open the named recording details.
- Web version changes recomputed subtitle cost notices and rejected responses
  for detached notices; track choices stayed attached to their file. (Removed
  with the web item-page restoration; the original page has no version
  selector.)
- Web home videos and recordings used the shared item composition, including
  the existing home-video metadata editor in More actions. (Removed with the
  web item-page restoration; they use their original bodies again.)

The original download action uses a dedicated authenticated, range-capable
attachment endpoint. Downloading bytes does not create a watching session.
