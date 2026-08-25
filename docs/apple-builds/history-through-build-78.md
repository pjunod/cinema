# Apple build history through build 78

Per-build narrative that accumulated in `clients/apple/README.md` and
`docs/APPLE-CLIENT-PARITY.md` before issue #509 moved it here. Every Apple
change used to prepend a sentence to both blockquotes at the same offset, so any
two concurrent Apple branches conflicted on both files, and the conflict
returned every time `main` moved.

Both documents narrated the same builds in deliberately different words. Both
runs are preserved verbatim below rather than merged, because rewording them
would be a content change disguised as a governance fix.

Builds from 79 on are recorded one file per change in this directory; see
[README.md](README.md).

## From `clients/apple/README.md`

Build 78 keeps the tvOS dashboard as Standard and adds shared Mini and full
Debug playback-info modes on iOS and tvOS. Build 77 carries the explicit
delivered-SDR acknowledgement required for a forced bitmap-subtitle session
over an HDR source, without allowing an unplanned HDR downgrade. Build 75 adds
online PDFKit reading on iPhone and iPad with exact-revision temporary bytes,
page resume, local search, and protected-document refusal. Build 74 consumes
Cinema's server-owned ebook format/action registry so detected-but-external
formats cannot acquire a false **Read** or offline action. Build 73 keeps
reconnect and Settings reachable when an offline download opens first. Build
72 displays bounded book author metadata and exact work-linked editions on
browse and detail surfaces. Build 71 keeps pending offline reading state
separate for each exact item/file/revision edition. Build 70 adds
profile-scoped, atomically published offline EPUB reading and newest-locator
replay on iPhone and iPad, with no tvOS action. Build 69 adds in-app online
EPUB reading on iPhone and iPad with an isolated, same-origin WebView,
memory-only authentication, cross-device locator resume, and no tvOS reader
action. Build 68 adds the revision-bound ebook reading-state wire models and
authenticated API routes. Build 67 gives stall recovery somewhere to go: a
reopen on a growing HLS session now names its exact predecessor with
`reopen_reason: "stall"`, so the server answers one rung *down* rather than
rebuilding the rung that just starved, states `quality_auto` so a subtitle
burn's promise height is not read as a sticky manual pick, and stops at the
ladder floor — the bound the server deliberately leaves to the client. Build
66 adds unattended physical-device bandwidth acceptance with exact runway
evidence. Build 65 keeps the delivery watchdog honest about a player that is
still making progress. Build 64 stops the delivery watchdog firing on a
healthy player: a full forward buffer looks exactly like a wedge from the
server's delivery meter, so the film clock and the buffered runway now have to
agree before it recovers. Build 63 makes stall recovery un-foolable: one
shared no-progress clock that regime flapping cannot reset, a longer bounded
leash (then a visible error) where recovery used to disarm forever, a
server-truth delivery watchdog fed by the 2-second status poll, and a rolling
budget that turns automatic reopen storms into the failure screen. Build 62
shows the audio and subtitle tracks a file actually has on the detail screen
and lets a viewer choose both before pressing Play. Build 61 corrects copy-HLS
recovery to seek past the preceding keyframe and adds correlated AVPlayer,
access-log, buffer, and server-supply evidence to every stall report. Build 60
requires a concrete PGS overlay track before treating a subtitle as an active
overlay, so ordinary playback with no selected subtitle keeps its AVKit PiP
controller attached. A physical H.264/ AAC baseline on iPhone Air (iOS 26.6)
and iPad Pro 13-inch (M4, iPadOS 26.6) reached active PiP and returned or
stopped cleanly; the separate in-app PGS refusal still needs physical
acceptance. Build 59 keeps the iOS Picture in Picture control reachable when
AVKit cannot start yet or an in-app PGS overlay blocks system output, bounds
the not-ready explanation to five seconds, and rejects stale availability from
a detached PiP controller instead of sending a start command through a nil
optional. Build 58 labels a recognized `pgs-v1` subtitle as an Overlay instead
of the legacy Burn-in fallback, so the physical HDR/Dolby Vision run in
[APPLE-PGS-OVERLAY-ACCEPTANCE.md](../../docs/APPLE-PGS-OVERLAY-ACCEPTANCE.md)
can distinguish the path it exercised. Build 57 makes the tvOS progress bar an
ordinary focus stop until Select engages scrubbing, so left/right can cross
the transport row without seeking. Build 56 restores bidirectional tvOS focus
between show/season header actions and their non-empty child shelves. Build 53
splits season episode cards into Play artwork and Details copy on iPhone/iPad;
tvOS keeps one lifted card whose Select action plays. Build 52 preserves
completed iPhone and iPad offline download locations across the equivalent
`/private/var` and `/var` container spellings returned by the system. Build 51
divides a long final audio tail into bounded, sample-preserving HLS segments.
A repeated near-end boundary now completes at the actual media position; a
genuinely early repeat stops with telemetry and **Try Again** / **Close**
actions instead of reopening forever. build `49` adds first-class audiobook
details and playback through the shared audio player and progress path;
physical-device acceptance remains pending. Build 46 adds a one-shot,
session-joined TTFF beacon when the online player first advances, with
separate cold-start and resume reasons; passing physical-device evidence
remains pending. Build 29 added app-managed offline viewing on iPhone and
iPad; the action remains hidden on tvOS.

## From `docs/APPLE-CLIENT-PARITY.md`

Build 78 keeps the ten-foot tvOS dashboard as Standard and adds the shared
Mini and full Debug playback-info modes on iOS and tvOS. Build 77 carries the
explicit delivered-SDR acknowledgement required for a forced bitmap-subtitle
session over an HDR source while refusing an unplanned HDR downgrade. Build 75
adds online PDFKit reading on iPhone/iPad with exact-revision temporary bytes,
page resume, local search, and protected- document refusal. Build 74 consumes
the server-owned ebook format/action registry and keeps external formats out
of the built-in and offline readers. Build 73 preserves the full tab shell
when offline downloads open first, so reconnect and Settings remain reachable.
Build 72 displays bounded book author metadata and only exact work-linked
editions. Build 71 scopes newest pending offline reading state to the exact
item/file/revision edition. Build 70 adds profile-scoped, atomically published
offline EPUBs, token-free local reading, and newest-locator reconnect replay
on iPhone/iPad. Build 69 adds in-app online EPUB reading on iPhone/iPad
through an isolated same-origin WebView with memory-only authentication,
shared locator resume, and no tvOS reader action. Build 68 adds revision-bound
ebook reading-state wire models and authenticated API routes. Build 67 adopts
the bound same-session stall reopen: a sustained stall on a growing session
names its exact `previous_session_id` with `reopen_reason: "stall"` so the
server answers one rung down, every create states `quality_auto`, and the
ladder-floor retry budget the server deliberately does not implement lives
here (§ Quality row). Build 66 adds unattended physical-device bandwidth
acceptance with exact runway evidence. Build 65 keeps the delivery watchdog
honest about a player that is still making progress. Build 64 corrects build
63's delivery watchdog, which fired on healthy buffered playback and
interrupted a 2160p session roughly every two minutes; the film clock and
buffered runway are now required to corroborate the server's delivery meter.
Build 63 hardens stall recovery against the tvOS freeze observed on 2160p
copy-HLS: a shared no-progress clock immune to `timeControlStatus` flapping, a
bounded unestablished leash instead of a disarmed detector, a server-truth
delivery watchdog on the status poll, and a rolling automatic-reopen budget
that ends storms at the visible failure screen. Build 62 lists the audio and
subtitle tracks a file actually has on the detail screen and lets a viewer
choose both before pressing Play (§6). Build 60 requires a concrete selected
PGS track before treating the overlay as active. Its physical non-PGS baseline
reached active PiP and returned or stopped cleanly on iPhone Air (iOS 26.6)
and iPad Pro 13-inch (M4, iPadOS 26.6); the separate PGS refusal remains
unverified on hardware. Build 59 keeps every supported-but-unavailable iOS PiP
state tappable long enough to explain it, including a detached AVKit
controller and the in- app-only PGS overlay. The not-ready explanation clears
after five seconds so it cannot pin the player banner or iOS system chrome.
Build 58 distinguishes a recognized `pgs-v1` Overlay from the Burn-in fallback
in the subtitle menu and ships the decidable physical-iPad run in
[APPLE-PGS-OVERLAY-ACCEPTANCE.md](APPLE-PGS-OVERLAY-ACCEPTANCE.md). Build 57
requires Select to engage tvOS progress scrubbing, leaving left/right free to
cross the transport row without a seek. Build 56 restores bidirectional tvOS
focus between show/season header actions and their non-empty child shelves.
Build 53 gives season episode artwork a direct Play action while the copy
remains Details on iPhone/iPad; tvOS keeps one lifted card whose Select action
plays. Build 52 preserves completed offline asset locations across the
equivalent `/private/var` and `/var` container spellings returned by the
system. Build 51 divides long final audio tails into bounded HLS segments,
completes repeated boundaries in the final 5% at their actual media position,
and gives genuinely early repeats a telemetered **Try Again** / **Close**
failure instead of another automatic reopen. Build 49 adds first-class
audiobook details and playback through the shared audio player and progress
path. Source and simulator coverage verifies audio-container direct-play
routing, resume/Start over selection, global progress, and missing-part
advancement; physical-device acceptance remains pending. The native scrubber
remains local to the current audiobook part and audiobook offline packages are
not yet supported. Build 46 also emits the Performance II N0 TTFF beacon at
the first advancing online frame, including the live HLS session id when one
exists and separating cold starts from resumes; passing real-device ingest
remains unclaimed.

## From `docs/STATUS.html` (the TestFlight upload item)

The `👤 Paul` upload item carried a parenthetical naming every feature waiting
to be uploaded. It grew with each build and is preserved here; the item now
points at this directory instead.

shared Mini/Standard/Debug playback info; online PDFKit reading with
exact-revision temporary bytes and page resume; server-owned ebook format
actions; offline-first launch navigation recovery; exact book metadata and
edition relations; exact-edition offline reading-state replay; profile-scoped
offline EPUB reading and newest-locator replay; in-app online EPUB reading;
revision-bound ebook reading-state API models; bound same-session stall reopen
that steps the quality ladder down with a client-owned floor retry budget;
unattended physical-device shaping with exact runway evidence; tvOS
stall-recovery hardening: shared no-progress clock immune to regime flapping,
bounded unestablished leash, server-truth delivery watchdog, rolling
automatic-reopen budget; plus detail-screen track facts with pre-play
audio/subtitle selection, copy-HLS keyframe-corrected stall recovery with
correlated client/server evidence, bounded final-audio segments, graceful
early-end recovery, audiobook support, episode media, in-window seek routing,
completed offline download location recovery, tvOS show/season shelf focus,
engage-to-scrub progress routing, unambiguous PGS overlay diagnostics, legible
PiP unavailability, and correct handling of an absent PGS overlay track
