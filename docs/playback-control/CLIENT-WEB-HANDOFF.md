# Web prepared-switch adapter — handoff

**Status:** built — ready to build, message plumbing only · **Exclusive lock:** you own
`crates/plurxd/src/web/index.html` for the life of this branch · **Repo:**
`noirr/plurx` on Forgejo, branch from `effort/streaming-reliability` ·
**Written:** 2026-09-08

Read `docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md` in the repo first — it is the
wire contract, verified against the server code, and shared with the Apple and
Android adapters. This document is only what is specific to the web player.
Where the two disagree, the contract wins.

**Standing instruction: this adapter changes no server *behaviour*.** The web
player lives inside the server crate, so you will be editing a file under
`crates/` — that is expected and is the one exception. But if a step seems to
require editing `crates/plurxd/src/playback_control.rs`, `http/hls.rs`, or any
other Rust file, stop and flag it.

---

## 1. Two things that make this one different

**You own `index.html` exclusively.** The embedded player, every settings
panel, the whole admin UI and this adapter all live in one ~20,000-line file:
`crates/plurxd/src/web/index.html`. Two branches editing it in parallel
conflict badly, so the Apple and Android sessions have been told not to touch
it. Hold that lock: keep your branch short, and if you need something from
elsewhere in the file, take it in the same PR rather than opening a second one.

**The web capability is `false` on measured evidence.**
`crates/plurxd/src/web/index.html:6792`:

```js
codecs,dynamic_ranges:ranges,dual_player_preparation:false};
```

Safari's codec/HDR case reached **13/20**. Apple declares `true` on its own
20/20 proof; Android is `false` after the Google TV failed 3/3.

**Do not flip it to make your adapter produce an offer.** That turns a refusal
you can see into a stall a viewer sees. The known problem is that a bare
boolean cannot say *yes for this recipe on this browser*, so a correct `false`
is also throwing away browsers that would pass — narrowing it is a **v1
protocol change** and deliberately not adapter work. If you conclude the
adapter is untestable without it, that conclusion is the deliverable: write it
up and raise it.

You build the plumbing and test it against a fake transport that hands you an
offer.

---

## 2. What nobody can demonstrate yet

**The staged successor has no producer.** `stage_prepared_successor` writes a
durable row and reserves the actor's slot and starts no encoder — its own doc
comment says so — and the staged route sits at the publication sentinel, so
fetching its playlist is refused until commit. That is a server gap blocked on
the open P2/D6 device measurement.

Every adapter therefore implements the message plumbing and stops: declare,
receive, acknowledge, commit, handle every refusal. Presentation — the second
`<video>`, the swap — is a later PR against a client already speaking the
protocol correctly.

---

## 3. Where the code is

All in `crates/plurxd/src/web/index.html`. Useful anchors as of this writing
(line numbers drift — grep the token, not the number):

| Anchor | What is there |
|---|---|
| `dual_player_preparation:false` (~6792) | The capability document the reporter sends |
| `reporter.bootstrap&&reporter.bootstrap.control_epoch` (~7042) | Where the control bootstrap is read |
| `request.control_epoch===waiter.controlEpoch` (~7103) | The exchange's own fence |
| `response&&response.action&&response.action.type==="terminal"` (~7192) | Action dispatch — where a `"prepare"` arm goes |
| `verdict&&verdict.type==="hold"` (~3700, ~10727) | The other action the player already handles |

Tests are node scripts, not a framework:

- `tests/playback/web-control.test.js` — the control reporter's contract.
  Line 131 currently asserts `dual_player_preparation: false`; keep that
  assertion and let it pin the value.
- `tests/web/settings-sections.test.js` — the settings panels, and the
  `shippedSource(fn)` harness that extracts a named function from the shipped
  HTML and runs it against stubs. Read that harness before writing tests; it is
  how everything in this file is tested and it has sharp edges (a stub that
  drops an argument silently passes — there is a comment in the file about
  exactly that bug).

Run them with `make web-check`, which runs every script under `tests/web/` and
`tests/playback/` plus the theme and contrast contracts.

---

## 4. The contract, in the order you will need it

Full detail in `docs/playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md`. The web-shaped
summary:

**Declare.** Add `"prepare_replacement"` to the reporter's supported-actions
list. Leave `dual_player_preparation` alone (§1). Capabilities are sent only
on `sequence == 1` and the server reads the **retained** copy, so this is
decided once per session.

`observed_download_bps` must be reported and must clear a **2× floor** against
the delivered rate the server measures — a nil reads as a refusal, not a pass.
Check what the web reporter sends today; if it sends nothing, that is a
prerequisite for this adapter and belongs in the same PR.

**Receive.** `action` becomes `{"type": "prepare", …}` with `action_id`,
`session_id`, `playlist_url`, `media_origin_ms`, and a complete
`effective_selection` (seven fields; `codec` is only ever `"source"` or
`"server_selected"`; `dynamic_range` is one of four strings or null).
`media_origin_ms` is the **source** position the successor's timeline calls
zero — not a playhead, not `currentTime`. Keep it exactly.

**The same offer repeats** on every accepted exchange while the slot stays
staged. Same `action_id` means same offer — do not create a second `<video>`
per exchange. It **expires** after about 330 seconds.

**Acknowledge.** States and required fields:

| `state` | Also required |
|---|---|
| `metadata_ready` | — |
| `buffer_ready` | `buffered_through_ms` |
| `committed` | `first_frame_unix_ms` **and** `committed_media_origin_ms` |
| `failed` / `aborted` | — |
| `switched` | `first_frame_unix_ms` — **do not send this** |

**The trap.** A `committed` whose `committed_media_origin_ms` does not equal
the offered `media_origin_ms` is **discarded, not refused**: `200`, nothing
commits, same offer returns. No error to catch — detect it by the offer's
persistence. Omitting the field entirely *is* a `400`. Three of the four rules
under that table behave this way.

This one bites the web player hardest, because `currentTime` is right there
and recomputing the origin from it looks natural. Echo the offer.

**Also:** the exchange carrying `committed` must repeat the same `selection`
the offer was staged against. The web player has a quality menu the viewer can
touch mid-preparation — if they do, abandon the offer rather than commit;
committing anyway is `409 stale_control` *and* tears the successor down.

**Never bundle `committed` or `switched` onto a request whose `demand` is
`end`.** The web player sends `end` on teardown and on tab close, which is
exactly where this is tempting.

**Retry by resending, not re-sequencing.** Equal `sequence` with a
byte-identical body is an idempotent replay returning the original answer;
equal sequence with a different body is `409 stale_control`. A `503` can
follow a durably applied commit, so replaying the same sequence is the only
safe recovery.

**A session binds to its first `client_instance_id`.** A second one is
refused `409 stale_control` for the life of the owner epoch — minting a fresh
instance to escape a refusal is a recovery path that can never succeed. This
matters more on web than elsewhere: a reload or a second tab must not
accidentally mint one against a live session.

---

## 5. Milestones

### 5.1 Declare

Add `"prepare_replacement"` to the supported-actions list. Nothing else.

**Acceptance:** extend `tests/playback/web-control.test.js` to assert the
request body carries it, and keep the existing
`dual_player_preparation: false` assertion so the value stays pinned.

### 5.2 Model the offer

A `"prepare"` arm in the action dispatch, reading all five fields and the
complete `effective_selection`.

**Acceptance:** a test over the exact JSON in the contract document, asserting
every field including nulls. A test that reads a partial object and passes is
not this test.

### 5.3 The acknowledgement ladder

`metadata_ready` → `buffer_ready` → `committed`, bound to the offer's
`action_id`, echoing `media_origin_ms` verbatim.

**Acceptance:** a test driving the ladder against a fake transport and
asserting the request bodies. Then the one that matters: a test where the
echoed origin is wrong asserts the client **notices the offer came back**
rather than waiting on an error. If it asserts a status code, it is testing
the wrong thing.

### 5.4 Refusals

Every row of the contract's failure table. `404 session_gone` — note the
status, it is not `410`. The three causes behind `409 stale_control` want
three different responses. `503` replays the same sequence.

**Acceptance:** a test per row asserting the player's next action. None may
end with a viewer on a spinner.

### 5.5 Teardown

An offer that expires, is withdrawn, or fails releases whatever was built for
it and leaves the playing `<video>` untouched.

**Acceptance:** a test asserting nothing survives and the exchange loop
continues.

---

## 6. Non-goals — do not do these

- **Do not flip `dual_player_preparation` to `true`.** §1. If you believe the
  adapter cannot be finished without it, write that up as the finding.
- **Do not send `switched`.** Grep your diff and say so in the PR.
- **Do not create the second `<video>` or build the swap.** Nothing to present
  until the successor has a producer. Leave a marked seam.
- **Do not use `localStorage` or `sessionStorage` for anything about this.**
  Control state belongs to the session, not the browser.
- **Do not edit any Rust file.** The one file you own is `index.html`.
- **Do not touch `clients/apple/` or `clients/android/`.** Two other sessions
  own those.

---

## 7. Delivery

Branch from `effort/streaming-reliability`, own PR into it, `WIP:` title
prefix until merge-ready, one adversarial review after that, findings
implemented, then the suites, then merge it yourself.

The web gate is `make web-check` — every script under `tests/web/` and
`tests/playback/`, plus the theme and contrast contracts. It must be fully
green; a skipped test is a failed test. The `effort web static contracts` CI
job runs the same thing.

Because `index.html` is embedded in the server crate, your PR also triggers
`effort Rust compile`. Run `cargo fmt --all` and
`cargo clippy -p plurxd --all-targets` before pushing even if you wrote no
Rust — the file is included at build time and a malformed edit fails the
compile, not the web check.
