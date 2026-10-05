# Clients — status history

**Status:** done · records moved verbatim from `STATUS.md` on 2026-09-24

**Moved here from [STATUS.md](../../STATUS.md) on 2026-09-24**, verbatim, by
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
M6. Each section keeps its original heading under the date it was first
recorded in `STATUS.md`; relative links are re-based to this folder and
nothing else changed. Newest first. These are records: a section here
describes the state on the day it was written, and
`tests/operations/test_status_pr_claims.py` keeps holding it to the same
merged-pull-request rule it held in `STATUS.md`.


## 2026-09-24 · Watch view: menus and Playback info escape the picture; the picture goes under the header

**Branch `fix/watch-popovers-escape-picture`, pull request open as a draft;
not merged, nothing deployed.** Paul's report of 2026-09-24 on the new web
watch view: the Playback info readout ran past the bottom of the picture and
was cut off there, the subtitle menu ran past its top so half of it could not
be chosen, and scrolling the page slid the picture over the main navigation.
One cause: the slot player's host was fixed at z-index 90 with a clip-path to
its own box. It now sits under the page's chrome (z-index 10) and clips
nothing; `positionMenu` / `positionStats` bound each popover to the viewport
minus the stuck chrome (`watchPopoverBounds`) and re-run on every scroll
frame. Record: [docs/clients/WATCH-VIEW-LAYOUT.md](WATCH-VIEW-LAYOUT.md)
(“The picture under the page; the menus and the readout over it”). Browser
acceptance extended with a thirty-track menu, the Diagnostics readout and a
scrolled-under-the-header check; passes on fine and coarse pointers.
Native clients untouched.

## 2026-09-22 · PGS subtitles stopped blocking the start path


Three pieces, from one report: *Bad Boys: Ride or Die* would not play on the
TCL tablet on 2026-09-21, and the overlay the viewer saw was the least
interesting of three failures that night. Diagnosis, measurements and the plan:
[docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md](PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md),
reviewed by Fable (APPROVE WITH CHANGES, folded in).

The measurement that explains all of it: file 5208 is a **79.5 GB** remux whose
PGS subtitle packets are interleaved across 116 minutes, so extracting one
track read the whole film — **402 s to produce 18,866 bytes**, which is just
the array at 198 MB/s.

- **#437** (`5c48ed5ab`, merged) — the cluster replacement gate is reclaimed on
  evidence, not on a clock: a hold that declared itself abandoned, or one past
  a 120 s ceiling, loses its player. Fixed the refusal the viewer quoted.
- **#445** (`5c605768`, merged) — a session start no longer awaits a full-film
  demux, and a pending sidecar is a named `startup_timeout` rather than a
  codeless 503 no client retries. Bounded for the start path **only**: offline
  restore, the VTT endpoint and package production keep their unbounded wait,
  because for them a slow success must stay a success.
- **#447** (`883cf4d42`, merged) — the PGS overlay is offered per caller rather
  than per node. `/decision` narrows on the server switch **and** the caller's
  own `subtitle_overlays` claim, so a client that cannot paint a bitmap is not
  offered a PGS default and is told the truth that selecting one burns the
  video. **This is what makes the gate safe to flip.** Before it, turning the
  gate on would have burned web direct-plays: the server stamped the PGS track
  `default`, and the web applies the server's default 400 ms after open, which
  for a bitmap track means a burn — or, on HDR, a degraded notice instead of a
  subtitle. Two deliberate limits, both from the adversarial review: **no caps
  document at all** falls back to the switch alone, because the legacy query is
  a mixed-fleet path both native clients reach on any 400/404/405 and reading
  silence as a refusal would send a capable client off to re-encode a whole
  film; and the `overlay` field on the track keeps the server's own answer,
  because it describes what this process can deliver rather than what this
  caller can paint. Item detail keeps answering `false`: it has no capabilities
  document, and the web's detail surface does not narrow the default by a
  renderer. `tests/validation/test_caps_wire_conformance.py` pins the field
  name across all four ports, because `DeviceCaps` has no
  `deny_unknown_fields`, so a misspelled claim is silently dropped rather than
  refused — costing a needless burn and, on an HDR source, the grade with it.

- **#453** (`1d21b184e`, merged) — overlay seeks and failures, on both native
  clients against one shared fixture (`tests/playback/pgs-overlay-cases.json`).
  Four bugs fixed: Apple's 1 s tick cancelled its own in-flight load, so any PNG
  slower than a second never arrived; Apple re-raised a notice every second
  after one failed window; Android left a finished cue on screen after a seek
  into the refresh margin; Android read every refused manifest as "empty". The
  server now answers a failed preparation with a typed
  `pgs_overlay_prepare_failed` instead of a 503 both clients kept polling for
  ten minutes; only capacity is a 503. Apple build 179, Android 119.
- **Fix C — every PGS track rides the fragment-index pass**, which already reads
  the whole file. Designed in #451 (§6 of the RCA, three adversarial rounds with
  ffmpeg experiments, each overturning something load-bearing), built as three
  PRs:
  - **#456** (`8962c0c66`, merged) — the store at
    `<cache>/runtime/subtitle-source-v1/` and its two readers. The overlay uses
    a stored `.sup` instead of demuxing; the burn path derives its `.mks` from
    it in a fraction of a second (with `-copyts` and **no** `-start_at_zero`,
    which would have moved every cue early) and answers "nothing to burn"
    without reading 79.5 GB for a track with no cues. MPEG-TS sources keep
    today's extraction.
  - **#466** (`cf5666876`, merged; first merged as #460, see below) — the
    producer: one `tee` output after the index's `pipe:1` with a `sup` slave
    and a `framecrc` companion per track, `onfail=ignore`, and a mandatory
    `null` sentinel, so the worst case is no subtitles and never no index; the
    index's argv, bytes, cache key and retry rules are unchanged, measured.
    Per-track verdicts from byte arithmetic, a persistent latch, a behavioural
    startup self-test, a local-disk and free-space gate, and one Developer
    switch (`subtitles.stored_sources`) that turns off producer and readers.
  - **#463** (`9236de83a`, merged) — attribution: the analysis row says the pass is also
    keeping N PGS tracks, by title, with bytes and a link to the switch, on both
    indexers; a Maintenance card shows the store, what is running and why.

**Forge anomaly, 2026-09-23 13:48 UTC.** Forgejo reported #460 merged as
`6a9a6a5a2`, but the server-side reflog shows `refs/heads/main` moved to it and
was set back to `8962c0c66` one second later by an internal "update by push".
Main never kept the merge. The same head was re-landed as #466. #460 had been
retargeted from its stacked base to `main` just before merging, which is the one
thing it did differently from every merge that stuck — worth avoiding
(merge stacks bottom-up and open the upper PR against `main` fresh) until the
cause is known.

**Deployed to all four nodes 2026-09-23 19:28 UTC** (`v0.3.0-3626-gfad591a46`, each node's own build report; the ride-along self-test passed on ffmpeg 8.1.2-Jellyfin). The first `deploy.yml` run called lab4 and lab6 "already at" that build while their containers were a day old, because their checkouts had moved without a rebuild; `-e force=true` rebuilt them, and `pjunod/ansible#4` now rebuilds whenever the running image's revision label differs from the checkout. **The mobile apps and the overlay switch are not done**: devices need Apple 179 / Android 119, then
the overlay's
enablement check — two devices, one Android and one Apple, one playing a DV
title and one an HDR10 title, a seek each way — before the gate
(`subtitles.pgs_overlay`) is turned on, and the Developer and Maintenance page
layout goldens, which need `scripts/ui-baseline --self-host --update` on a
machine with Playwright (the golden is also stale on `main` for unrelated
routes).

## 2026-09-19 · The web app is a tree, and the bytes are the same ones

**[#369](http://forge.lan:3000/noirr/plurx/pulls/369) (one line, M0) and
[#371](http://forge.lan:3000/noirr/plurx/pulls/371) (M1–M6) from
`agent/web-shell-split` into `main`; three adversarial reviews done, five
findings folded.** `crates/plurxd/src/web/index.html` was 23,901 lines — one
3,209-line `<style>` block, ~36 lines of markup, a 218-line theme engine in
`<head>`, and a 20,433-line application in `<body>` with 1,152 top-level
functions. It is now a 97-line shell and **sixty-two files** served from one
ordered table, mapped by
[docs/clients/WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md). Still no
bundler, no build step, no ES modules: plain `<script src>` scripts sharing one
global scope, exactly as the inline script did.

**Nothing else changed, and that is the claim the PR is built to prove.** A
one-shot `web-shell-identity` reassembled the tree from the shell's own tag
order and compared it byte for byte to the pre-split file — 3,209 lines of CSS,
218 of theme engine, 20,425 of application, identical. **Exactly one block
moves**: the eight lines registering `LAYOUTS.classic.chrome`/`.views` execute
at load and name five functions declared later, so cut at the section banners
the app dies with `ReferenceError: classicItemBody is not defined`. They become
`layouts/register-classic.js`, served between catalog and theater — still
before the `applyLayout()` that paints the first frame. `make web-check`'s Node
entries fail on the same six tests, **by name**, as they do on `main`.

Serving is `WEB_ASSETS` plus a SHA-256 per row applied to the shell's tags at
startup: assets are `immutable` for a year, `/` is `no-cache`, and
`/assets/<unknown>` is a **404** where it used to be `200 text/html` — which a
browser then tried to execute as JavaScript.

**What the reviews caught is the part worth reading.** Three guards had
stopped being able to fail. `theme-family` was refusing a CSS selector in a
JavaScript file, so dropping Copper from all 54 cockpit rules stayed green. Two
ordering claims compared offsets into a string where the markup always precedes
the rows, so moving `cluster-panel.js`'s tag to the last line of `<body>` —
after every row that reads it — also stayed green. The surface fence went
**silently vacuous** on the split and the first repair was the same mistake one
folder smaller: scoped to `player/*.js`, a rogue
`document.getElementById("psurface").innerHTML=` in `core/chrome.js` still
printed PASS. It globs the whole tree now, shell included, and fails closed if
the glob shrinks.

And the static order gate had a real double-miss: it skipped every function
expression, so an immediately-invoked `.forEach` callback inside a branch the
`vm` stub does not take passed **both** gates while shipping a blank page to
every touch device. It enters immediate callbacks now, and models `class
extends`, computed keys, static blocks, default parameters and destructuring
defaults. Nineteen shapes of forward reference are required to be caught and
five deferred shapes required not to fire — a gate that fails on a click
handler is a gate people route around.

Two things genuinely are different and are written down rather than left to be
discovered: a load-time throw no longer takes the rest of the app with it (a
half-working UI where there used to be a loud blank page), and `typeof` on a
later row's `let`/`const` now returns `"undefined"` where it used to throw.
Neither is reachable today. The 114 `index.html:NNNN` citations across 39
documents were **not touched** — they resolve through the layout table's
old-line column, and `rg 'index\.html:[0-9]+' docs/` counts 115 on both
branches.

## 2026-09-16 · The web item page is back to its pre-#317 layout

**[#342](http://forge.lan:3000/noirr/plurx/pulls/342) from
`revert/web-item-page-pre-317` into `main`; adversarial review done and its
findings folded; the Hiqlite fix it uncovered merged first as
[#340](http://forge.lan:3000/noirr/plurx/pulls/340) (`a6996538`).**
Paul compared real renders of the web item page before PR #317, after PR #317
and at current main and chose the pre-#317 page. The branch removes the shared
"viewing" item body and its styles so every layout renders its own original
item body again; the item route renders pixel-identical to the pre-#317 tree at
1280 px for movie, series, season and episode against the ui-baseline fixture
library. Grid and rail poster cards were never changed by either PR. Native
item pages are out of scope and keep the PR #330 design. Regenerating the
structural golden for the restoration turned up a real bug on `main`:
`GET /api/v1/dvr/attention` answers 500 on every replicated store because the
Hiqlite page statement named its placeholders out of order — fixed and merged
as #340 with the assembled-statement census extended to cover it. The committed
`tests/ui-structure.golden` still predates PR #317 and every route's header
chrome has drifted since (Recordings link, DVR indicator, mobile search); that
regeneration is a separate chore, not part of these two PRs.

## 2026-09-03 · The ✕ on an iPhone could not leave a film

**PR [#853](https://github.com/pjunod/plurx/pull/853) — MERGED to main as `e31a6cb4`, 2026-09-03, branch `fix/close-control-exits`, fix commit `0f904213`.** Paul: "the x to
close out media playback does not work on apple devices. There's no way to
get out of the movie except force close." Confirmed at source, not on a
device: the iOS ✕ fed `back` to the touch routing table, and `back` while
chrome is visible is `hide` — right for a key, wrong for the one button whose
purpose is to leave. So the ✕ hid the chrome in `transport` (the state it is
tapped from), closed the panel in `info`, cancelled in `scrub`, and exited
only from `hidden` and `failed`, where it is not drawn. Android's phone back
arrow had the same fault in both `PlayerScreen` and `OfflinePlayerScreen`;
the system back gesture there still left after two presses, which is why
only Apple was reported. The web's `✕ Close` calls `closePlayer()` directly
and was never affected.

The contract now says what its own touch note already claimed: the ✕ is the
`close` control, not `back`. `close_control` in
`tests/playback/player-input-contract.json` gives it one outcome list per
state — close whatever is open, then `exit`, every row ending in `exit` —
transcribed as `PlayerInputRouting.closeSteps` (Apple) and
`PlayerInputPolicy.closeSteps` (Android), each checked against the fixture
by its client suite; the JS contract test pins the fixture's shape, and the
Apple suite pins the call site (the ✕ and the failure view's Close run
`closePlayer()`, and nothing in `PlayerView` manufactures a `back` press).
Apple build 115, Android versionCode 70 (main took 114/69 while this was open). Swift and Kotlin compile only on
CI; `make web-check`'s player suites and the input fence are green in the
clone. **Not run on hardware** — the device pass is the iPhone/iPad ✕ from
transport and mid-scrub (one tap exits), from info Standard and Debug (the
panel's backdrop takes the first tap, the second exits — the ✕ sits under
the panel by design), and the Android phone arrow from transport and
mid-scrub.

## 2026-09-03 · Activity's Now playing row, read as a card

**PR [#849](https://github.com/pjunod/plurx/pull/849) — merged to main as
`a94a32cb`, 2026-09-03.** Paul asked for the activity status display to be "a
lot nicer": the Stream cell was one " · "-joined sentence of every session
fact, with the three things an operator brings to the page — is it playing,
is the server keeping up, is it held and why — buried among sequence
numbers. The cell now leads with a state pill, then named meters under the
player info panel's own labels (Position, Server ahead, Demand window
against target with a fill bar, Client runway, Suspends, Delivery rate),
then a "Technical details" disclosure carrying everything the sentence used
to say, which stays open and keeps keyboard focus across the repaint. One
adversarial review, nine findings, all implemented — the two that mattered:
"Server ahead" was about to show the demand window under the player's
label for the physical reserve (now two meters, matching the player), and a
failed producer would have worn a green Active pill (failures now outrank
everything but a dead lease). Also found in passing: the rung and encoder
were read off `deliveries[]`, which never carries them, so "1080p · vaapi"
had never rendered against a real server. `make web-check`'s node suites,
`js-check` and `contrast-check` all green in the clone; 27 painter tests.
Web-only — nothing to deploy to the nodes beyond the next server image;
the Activity golden has no live streams so `ui-check` is unaffected.

## 2026-09-02 · The picker says which machine again

**[`agent/discovery-machine-name`](https://github.com/pjunod/plurx/pull/839),
open into main, 2026-09-02.** Every node of one logical server reports the
same `server.name`, so since `4a7ead78` (2026-08-20) a clustered node
advertised that bare name plus twelve characters of its node id — three rows
of `plurx · 5deeeebc8f39` in the Apple TV picker, on a fleet whose machines
have perfectly good names. It regressed `474e2ee1`, and neither
`deploy/README.md` nor the compose comments ever stopped promising
`lab6 · 10.42.1.20`; the discovery companion still runs `uts: host`
specifically so it can read the machine hostname.

The name a node computes for itself is what it advertises now, in both
advertisement paths. The node-id suffix survives as the fallback for a node
that has neither a hostname nor a LAN address — that is the DNS-SD uniqueness
the suffix existed for, and it is the only case that ever needed it, since a
hostname is unique on one LAN and an address is unique by construction. The
full node id stays in the TXT record and in the per-node host record either
way, so nothing that resolves a node loses information.

Three tests, one of them on the call site: reverting the branch that handed a
clustered node its bare `server.name` fails on any host that has a hostname or
an address (mutation-checked — it comes back `plurx · 6b98c6cb8388` against an
expected `vm · 192.0.2.2`). Server-only: no client rebuild, and the fleet needs
a redeploy before a TV shows the difference.
