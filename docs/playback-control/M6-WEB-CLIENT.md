# M6 web client — the prepared replacement path in the browser

**Status:** built · **Executes:** the web half of M6 · **Written:** 2026-09-07

Companion to [M6-CALLER-HANDOFF.md](M6-CALLER-HANDOFF.md) (where the
preparation decision is made) and
[M6-IMPLEMENTATION-HANDOFF.md](M6-IMPLEMENTATION-HANDOFF.md) (the prepared
recipe and its measured numbers) — this is *what the browser does with a
staged successor, and how an operator turns it on*. The server half is
described in [PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md); it was
finished before any client named the action, which is why it had never run.

## 1. What landed

`crates/plurxd/src/web/playback-control.js` and the player half of
`crates/plurxd/src/web/index.html` now implement the whole client side of a
two-player replacement:

| Step | What the browser does |
|---|---|
| declare | `supported_actions` carries `prepare_replacement` on every exchange |
| parse | a `prepare` action is validated whole — playlist, origin, selection — before anything is built |
| build | a second HLS pipeline on the successor's playlist, muted and `display:none` |
| align | the successor's local zero is placed at `media_origin_ms` in film time |
| report | `metadata_ready`, then `buffer_ready` with `buffered_through_ms` |
| switch | successor becomes visible and audible; predecessor is retired |
| commit | `committed` with `first_frame_unix_ms` **and** `committed_media_origin_ms`, sent only after a frame renders |
| settle | `failed` or `aborted` on every abandon path, never silence |

**A commit owes two fields, and the contract names one.** `committed` requires
`first_frame_unix_ms` **and** `committed_media_origin_ms`, and the second must
equal the offer's `media_origin_ms` verbatim: `ActionAcknowledgement::validate`
answers `400 acknowledgement.committed_media_origin_ms` without it, and
`bound_preparation_acknowledgement` silently drops a commit that names a
different origin. It is how the server tells *the client built what I offered*
from *the client built something else and is asking me to publish it* —
`desired_digest` already covers quality, codec, grade and subtitles, but not
position. A 400 is neither retryable nor a transport error, so a client that
omits it stops its own reporter for the rest of the session, immediately after
swapping the picture.

**The name trap, because it is the one mistake that fails silently in both
directions:** the string declared in `supported_actions` is
`prepare_replacement`; the string that arrives as `action.type` is `prepare`.
`accepts()` on the server is a literal comparison, so a client that declares
the tag is never offered anything, and a client that switches on the declared
name never fires. Neither produces an error anywhere.

## 2. Turning it on — Settings → Developer

The capability is not a code gate. `dual_player_preparation` is a switch on
the **Enable prepared quality handoff** card, and the readiness list beside it
is advisory: it says what enabling costs and whether each part is true right
now, and it does not block the switch.

```
 Settings → Developer → Enable prepared quality handoff
   ├── What must be true first    six rows: met / not met / not measured here
   └── [ ] Tell the server this browser can prepare a second player
```

**What the switch does:** sets `capabilities.dual_player_preparation` on this
browser's control exchanges. That is Gate A — the server stages nothing at all
for a client that says `false`, and the retained capability document is the one
it reads, so the switch applies to the next stream you start rather than to the
one already playing.

**Why it is per-browser and not a server setting:** the field is a claim about
one browser on one machine. A fleet-wide value would be a claim about all of
them — including, on web, every browser that is not the one that was measured.
It is stored in `localStorage` under `plurx.prepared_handoff`.

**How to read the readiness list:** *met* is a fact this page checked. *not
met* is a fact this page checked and it is false. *not measured here* is a
question a page cannot answer about a fleet, and is never rendered as a
refusal.

## 3. What actually happens when you enable it today

Nothing reaches a viewer, and the reason is worth stating exactly, because it
is not the one the client briefs give.

**Staging starts no worker.** `stage_prepared_successor`
(`crates/plurxd/src/http/hls.rs`) mints an incarnation and a session, writes a
durable `MediaSessionPreparation` row at `MEDIA_SESSION_PUBLICATION_BLOCKED`,
takes the actor's preparation slot and arms the 330-second deadline. It creates
no transcode and no VOD generator.

A staged route therefore classifies as `OwnerTransition` on every request —
`classify_durable_route` sends any row whose `publication_ready_at_ms` is not
`0` down that branch — and the playlist answers:

```
 GET /api/v1/hls/<staged>/index.m3u8
        │
        ▼
 relay_if_remote ──▶ classify_durable_route ──▶ OwnerTransition
        │
        ▼
 503 media_owner_transition       for the whole 330 s lease
        │
        ▼  (deadline reaps the row)
 410 media_session_ended
```

Commit does not fix it either: `PreparationExecutor::commit` runs the store CAS
that moves the playback pointer and retires the predecessor, and never creates
the worker. Unblocking the publication sentinel is a separate step driven only
from the live-worker lease loop, which a worker-less row never enters.

**So a client that builds a second pipeline today reaches `failed`, not
`buffer_ready` — on every platform, Apple included.** That is the first row of
the readiness list, and it is why the card still says *do not enable this for
viewers yet*.

The web client's behaviour on that path is correct, tested, and deliberately
does not repeat itself. A fatal manifest error settles the staging with
`failed` and frees the slot rather than holding the session's only preparation
slot until the deadline — and a successor that was **never playable** also
withdraws the offer: `preparedHandoffOffered` reports
`dual_player_preparation` false for the rest of that playback, so the server
stops staging. Without it, an operator who turned the switch on would get a
doomed second pipeline built on every quality change, which is a regression and
not a feature. It is learned, forgotten when the player ends, and needs no
flag; Apple's `PreparedReplacementCoordinator.canOfferPreparation` is the same
rule in the same place.

## 4. The gates that keep this honest

```bash
make web-check                              # the local gate for any web change
node tests/playback/web-control.test.js     # the control plane, alone
node tests/web/settings-sections.test.js    # the Developer card and its copy
scripts/control-reporter-browser-check      # a real browser, a real exchange
```

`tests/playback/web-control.test.js` carries the literal-JSON `prepare` fixture
asserted key by key — the payload nothing in the repository pinned before this
milestone — plus the second-pipeline harness: prepare, repeat, supersede,
abandon, fail, switch, commit. `scripts/control-reporter-browser-check` is the
one gate that *executes* the module rather than reading it as text; it now
requires three exchanges, so a browser that parses the module but refuses the
preparation fails there and nowhere else.

The node suites compose shipped functions with `new Function` over extracted
source text. That proves the function's logic and not that the page wires it
up, which is exactly how the web control plane once shipped a fully green suite
over zero completed exchanges. Run the browser check before believing the path
works end to end.

## 5. The switch moves the listeners, not just the id

Worth knowing before changing anything in this path, because it is the part
that has nothing to do with the protocol and everything to do with the page.

The successor primes on its own hidden `<video>`, so committing has to change
which element the page treats as `#video`. Swapping the two elements' ids fixes
every `getElementById("video")` — and fixes nothing else. `wirePlayer` runs
**once per page** and binds about twenty-five listeners to the element it
captured, and `play()` binds five more per playback:

```
 wirePlayer()            once per page   ─┐
   playing / waiting / error              │  bound to ONE element,
   pause / play / seeking / seeked        ├─ and `PLAYER_WIRED` never resets,
   timeupdate → pbTick, checkMarkers      │  so one handoff would brick the
   click / dblclick / pointerdown         │  player until a page reload
 play()                  per playback    ─┤
   onended → handleEnded                  │
   progressTimer → playbackProgressTick   │
   armHitchDetector · setupAirplay        │
   probeDecode                           ─┘
```

So the media listener set lives in `wirePlayerMedia(v)`, keyed to the element
rather than to the page, and `adoptPlaybackMediaElement(p,v)` moves it and the
per-playback bindings onto the element that now owns the picture. Then the
retired element **leaves the page**: keeping it as the next successor's host
would put a hidden pipeline behind the visible stream's overlays — dismissing
its loading spinner, raising its "Buffering…", reporting its errors as the
viewer's.

Two things the commit deliberately does *not* do. It does not begin a new media
attachment: `startPlaybackControl` captured the current one and refuses to
snapshot against any other, so a new token would wedge the reporter and the
`committed` this switch just earned would never reach an exchange. And it does
not release the predecessor's session — the commit CAS retires it server-side,
and releasing it here would remove the row the CAS names.

## 6. Non-goals

- **The capability literal is not a fleet claim.** The switch says what one
  browser will offer. Nothing in this milestone changes what
  [M6-IMPLEMENTATION-HANDOFF.md](M6-IMPLEMENTATION-HANDOFF.md) records as
  measured.
- **No new axis.** `PREPARED_AXIS_SETS` admits resolution/bitrate, and that pair
  with delivery method — both same-codec. A codec or dynamic-range change is
  never prepared, on any platform.
- **No `commit_replacement` action.** Commit and abort are acknowledgement
  states; the server never orders the switch and cannot. A design that waits
  for one waits forever.
- **No build step.** The web app is one `include_str!`-embedded file. A
  JavaScript syntax error in it compiles, links, passes every Rust test and
  then serves a blank page — which is what `make web-check` and
  `scripts/js-check` exist for.
- **`localStorage` holds the switch, never the preparation.** Preparation state
  is per-stream and must not survive the page; the switch is a per-viewer
  preference and must.

## 7. For the Apple and Android client sessions

Three corrections to the shared wire contract, found while building this half:

0. **A `committed` carries `committed_media_origin_ms` as well as
   `first_frame_unix_ms`**, echoing the offer verbatim. §C7's table omits the
   field entirely. Without it every commit is a `400`, and a 400 stops the
   reporter permanently — after the client has already switched the picture.
   This is the single highest-cost omission in the contract; §1 above has the
   citations.
1. **§C2 and §C8's "real encoder" is wrong.** Staging writes a durable row and
   takes the actor slot; it starts nothing. §3 above has the trace and the
   status code. Build the `failed` path first — it is the one your client will
   actually exercise.
2. **Both `master.m3u8` and `index.m3u8` are legal**, query and fragment are
   allowed and ignored, and the comparison is against the successor's own
   `session_id`. Validate the whole action before building anything; a
   preparation half-understood is worse than one refused, because the server is
   holding a slot for it either way.

**And one thing the contract's own §C9.2 got right yesterday and wrong today:
VOD is no longer excluded.** `f2fecc98` ("measure what a VOD session actually
delivers") populates `delivered_bps` for VOD sessions, which is what made the
throughput floor unreachable on the presentation nearly every session uses. A
client that gates preparation on "not VOD" now disables the feature on the
presentation the server just enabled it for. The doc comment on
`FallbackReason::ThroughputUnreported` still says otherwise; it is stale.

Everything else in the contract matched the tree at `main` when this was
written: the five acknowledgement states, the `committed`-may-not-ride-`end`
rule, the 330-second deadline, and the throughput floor itself — this client's
measured rate must be at least twice the delivered one, and web is the only
port that reports a rate at all.
