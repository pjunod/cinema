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
| commit | `committed` with `first_frame_unix_ms`, sent only after a frame renders |
| settle | `failed` or `aborted` on every abandon path, never silence |

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
viewers yet*. The web client's behaviour on that path is correct and tested: a
fatal manifest error settles the staging with `failed` and frees the slot,
rather than leaving the session's only preparation slot held until the
deadline.

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

## 5. Non-goals

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

## 6. For the Apple and Android client sessions

Two corrections to the shared wire contract, found while building this half:

1. **§C2 and §C8's "real encoder" is wrong.** Staging writes a durable row and
   takes the actor slot; it starts nothing. §3 above has the trace and the
   status code. Build the `failed` path first — it is the one your client will
   actually exercise.
2. **Both `master.m3u8` and `index.m3u8` are legal**, query and fragment are
   allowed and ignored, and the comparison is against the successor's own
   `session_id`. Validate the whole action before building anything; a
   preparation half-understood is worse than one refused, because the server is
   holding a slot for it either way.

Everything else in the contract matched the tree at `main` when this was
written: the five acknowledgement states, the `committed`-may-not-ride-`end`
rule, the 330-second deadline, and the throughput floor that keeps a prepared
handoff off every VOD session on every platform.
