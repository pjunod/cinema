# M6 web client — the prepared handoff in one embedded file

**Status:** ready to build · **Executes:** the web half of M6 · **Written:**
2026-09-07 · **Baseline:** effort head `e3b11182` on
`effort/decoder-selection-recovery`

This file is self-contained; you need no other document to start.
**Part I** is what to change in the web client, in what order, and how to know
it worked. **Part II** is the wire contract the server already speaks — the
same text ships in the Apple and Android briefs, and the `§C`-numbers
throughout Part I point into it. Apple and Android are being built in parallel
by separate sessions; do not change anything under `clients/`.

**Read Part II first.** In particular §C5, the `prepare_replacement` /
`prepare` name trap, which is the one mistake that produces a silent no-op
rather than a failing test.

**Standing instruction.** Every path and line number below was read out of the
tree at the baseline above. Re-verify before editing; `index.html` is 16,000+
lines and moves. If a step seems to require changing `dual_player_preparation`,
adding an axis to `PREPARED_AXIS_SETS`, or touching anything in `crates/` other
than `src/web/`, **stop and flag it** — those are §C13 non-goals and
they belong to nobody's client brief.

---

# Part I — the web work

## 1. What you are building, and what will not happen when you ship it

You are building the web client's half of a two-player replacement: declare the
vocabulary, accept a `prepare` action, stand up a second HLS pipeline on the
successor's playlist, align its timeline, report readiness, switch, and commit.

**And on the day it ships, no viewer will ever see it fire.** Web's
`dual_player_preparation` is `false` (`crates/plurxd/src/web/index.html:6690`),
measured on hardware, and the contract's §13 forbids you from changing it. Gate
A therefore refuses before Gate B is ever consulted, so the server stages
nothing for a browser.

This is deliberate and it is the whole reason the work is worth doing now.
§C14 has the finding: **Safari's measured failure was codec/HDR, and
the only axes the server will ever prepare are same-codec, where Safari reached
20 consecutive.** The `false` is protecting against a case the server cannot
offer. When a narrower, axis-keyed capability lands — a protocol v2 decision,
not yours — the platform that already has a tested client path is the platform
that ships that day. The platform that does not, spends a release catching up.

So your acceptance is tests, not a viewer-visible behaviour, and §7 says so
precisely. Write the PR description to say it too; a reviewer who expects a
demo will otherwise read a complete milestone as an unfinished one.

---

## 2. The web client as it stands

**There is no build step.** The entire app is one file,
`crates/plurxd/src/web/index.html` (16,396 lines), embedded into the daemon
with `include_str!`. There is no bundler, no module graph and no minifier. A
JavaScript syntax error inside it **compiles, links, passes every Rust test,
and then serves a blank page** — which is why `make web-check` exists and why
the Makefile says so in a comment at `Makefile:430-433`.

The control plane is a separate sibling file,
`crates/plurxd/src/web/playback-control.js` (411 lines), which is the
**reference implementation the Apple and Android ports are held to**. Changes
you make to its shape are changes two other sessions will mirror. Keep the
structure recognisable.

| piece | where | state |
|---|---|---|
| the video element | `index.html:3193`, `<video id="video">` | exactly one, in the player modal |
| the HLS instance | `PLAYER.hls`, a **scalar slot** | one at a time by construction |
| attach | `attachHls(video, playlistUrl, startAt)`, `index.html:6336` | **tears the incumbent down first**, `:6337` |
| the one attach call | `index.html:7174` | `attachHls(v, info.playlist_url, into)` |
| teardown | `index.html:8885-8892` | destroys and nulls `PLAYER.hls` |
| declared actions | `playback-control.js:19` | `["hold","retry_resource","terminal"]` |
| action validation | `playback-control.js:118-126` | shape check per type |
| action handling | `playback-control.js:285-296` | `retry_resource` paces; `terminal` stops |
| `acknowledgement` | `index.html:6802` | hard-coded `acknowledgement:null` |
| capabilities | `index.html:6689-6690` | `dual_player_preparation:false` |
| media origin | — | **does not exist**; §4 |

**`PLAYER.hls` is a scalar, and `attachHls` opens by destroying.** That single
design fact is the largest structural change in this brief: a prepared handoff
needs two live pipelines, and every path in the file assumes one. The comment
at `index.html:6337` records why the teardown-first rule was introduced —
overwriting `PLAYER.hls` used to strand an instance — so do not simply relax
it. §5.2 says what to do instead.

**Transport.** HTTP polling to `/api/v1/hls/{id}/control` on the server's
published cadence, driven from `playback-control.js`. There is no socket.

**One thing web already does that neither native client does:** it fills
`observed_download_bps`, from `hls.bandwidthEstimate`. §C9.2 makes that
load-bearing — a client that reports no throughput can never be offered a
preparation — so web starts this milestone with the half of the throughput
floor it owns already satisfied. Do not regress it; assert it on the serialized
body (§C12.8).

The half web does *not* own: `delivered_bps` is `None` on every VOD session
(§C9.2), so a prepared handoff cannot fire on VOD on any platform. For
web that is academic today — Gate A is closed anyway — but it is the reason a
manual VOD experiment will show nothing, and it is worth knowing before someone
spends an afternoon on it.

---

## 3. The tripwires

Three things in the tree will break, or fail to break, in ways worth knowing
before you start.

**`tests/web/settings-sections.test.js:276`** pins the Settings > Developer
copy verbatim, including `Android and web remain unqualified`. It is asserting
the *current* honest state and it should keep asserting it — your work does not
make web qualified, because qualification is a hardware measurement. If you
find yourself editing that assertion, you have talked yourself into changing
the capability literal. Stop.

**`tests/playback/web-control.test.js:745-751`** asserts the declared vocabulary and
**will fail** the moment you add `prepare_replacement`. That failure is
correct; update the expectation, do not delete the test.

**`scripts/control-reporter-browser-check` exists because this test harness
once shipped a fully green suite over a control plane that completed zero real
exchanges.** The node tests compose shipped functions with `new Function` over
extracted source text; they prove the function's logic, not that the page wires
it up. Run the browser check before you claim the path works end to end, and
say in the PR that you did.

**Harness detail that eats an hour:** the node tests build a scope by
concatenating extracted source fragments, and `shippedSource()` may end inside
a trailing `//` comment. **Join fragments with `\n`**, never with `;` or an
empty string, or the next fragment is swallowed by that comment and the test
fails somewhere unrelated.

---

## 4. The gap that is not on the wire: web has no media origin

Apple has `sessionMediaOriginMs` / `sessionAttachSeekMs` / `baseMs`
(`clients/apple/Sources/PlayerController.swift:5297-5311`); Android has
`clients/android/app/src/main/java/tv/plurx/app/player/MediaOrigin.kt`. **Web
has neither.** Its only origin-shaped concept is `t.offset =
info.start_seconds || 0` at `index.html:7146`.

`Prepare.media_origin_ms` is the exact source position the successor's
session-relative zero maps to (§C6). Without it you cannot align the
second timeline with the first, and the commit boundary is expressed in film
time — so a handoff that ignores it lands the viewer at the wrong second and
looks like a seek bug, not an alignment bug.

**Build the concept before you build the handoff.** A small, testable pair of
pure functions mirroring the other two clients, so all three stay comparable:

```js
// Source position represented by a session's own zero.
function sessionMediaOriginMs(hls) { … }   // vod → 0; else media_origin_ms,
                                            // else start_seconds * 1000; clamp ≥ 0
// Film time from a player's local time.
function realMediaPositionMs(localMs, baseMs, isVod) { … }
```

Name them after Android's (`MediaOrigin.kt`) rather than inventing a third
vocabulary. This is the one place where copying a sibling client is the right
instinct.

---

## 5. The work, in order

Each slice is independently reviewable and ends with an acceptance that is a
command or an assertion.

### 5.1 Declare the vocabulary and parse the action

`playback-control.js` only. No player changes.

1. `SUPPORTED_ACTIONS` at `:19` becomes
   `["hold","retry_resource","terminal","prepare_replacement"]`.
2. The action validator at `:118-126` gains a `prepare` arm — note the tag is
   `"prepare"`, §C5 — checking `action_id` parses as a UUID,
   `session_id` is a non-empty string, `playlist_url` is node-relative
   (§C6: exactly `/api/v1/hls/{session_id}/index.m3u8` or
   `…/master.m3u8`), `media_origin_ms` is a finite integer `≥ 0`, and
   `effective_selection` validates against §C9's ranges.
3. A malformed `prepare` stays fatal, exactly like any other malformed action.
   Do not soften it into an ignore — a preparation you half-understood is worse
   than one you refused.

**Acceptance:** `node tests/playback/web-control.test.js` passes with a new
literal-JSON fixture (§C12.2) and an updated vocabulary assertion, and
a negative test proves an absolute `playlist_url` is refused **without a
network request**.

### 5.2 A second pipeline slot

`index.html`. This is the structural slice and the one to review hardest.

`PLAYER.hls` stays the authoritative incumbent. Add a **separate, explicitly
named** slot — `PLAYER.preparedHls` — and a builder that does *not* go through
`attachHls`, because `attachHls`'s first act is to destroy the incumbent
(`:6337`) and that is exactly what must not happen.

```
 PLAYER.hls          ── authoritative, audible, visible ──▶ <video id="video">
 PLAYER.preparedHls  ── muted, detached, priming ────────▶ second <video>,
                                                            display:none
```

Rules that are not negotiable, each with its reason:

- **The successor is never audible or visible before the switch** (§C13). Two audio streams is the failure mode viewers report as "echo" and
  nobody can reproduce.
- **One successor at a time.** A second `prepare` with a different `action_id`
  while one is live is an `aborted` on the old one, then a build of the new.
  A repeated `action_id` is *the same preparation* and must not build twice
  (§C12.7).
- **Every exit path frees the slot.** Player close, session end, navigation
  away, quality change, seek. A leaked prepared instance is a second decoder
  running behind a closed modal.
- **Reuse the teardown discipline at `index.html:8885-8892`**, don't invent a
  second one.

**Acceptance:** `make web-check` green; a test drives prepare → abandon and
asserts both that the prepared instance was destroyed and that `PLAYER.hls` was
never touched.

### 5.3 Alignment

Implement §4's functions, then align the successor: its local zero corresponds
to `Prepare.media_origin_ms` in film time, and the incumbent's current film
position is what the successor must be primed to reach.

**Acceptance:** unit tests over the pure functions covering VOD, live-recovery
with a non-zero origin, and the `start_seconds`-only fallback. No player
required.

### 5.4 Readiness and the acknowledgements

`index.html:6802` currently hard-codes `acknowledgement:null`. Replace it with
a slot the preparation state machine fills.

| when | send |
|---|---|
| successor's manifest parsed, tracks known | `metadata_ready` |
| successor buffered past the intended switch point | `buffer_ready` + `buffered_through_ms` |
| successor visible and audible, predecessor retired | `committed` + `first_frame_unix_ms` |
| build failed | `failed` |
| viewer seeked, changed quality, or closed | `aborted` |

Read §C7 for the validation rules; three of them are `400`s a client
can only hit by being careless. The one worth restating: **a `committed` may
not share an exchange with `demand: "end"`.** On player-close-after-switch,
send the commit, then end, as two exchanges.

**Acceptance:** a test per row, asserting the **serialized** request body, plus
one asserting the `end`+`committed` combination is never constructed.

### 5.5 The switch

Swap the prepared instance into the authoritative slot: unmute, make visible,
retire the predecessor through the existing teardown, then send `committed`
with the wall-clock time of the first qualifying frame.

**Order matters.** The commit is a claim that the switch already happened. A
commit sent before the frame renders is a lie the server writes to a durable
row, and the CAS that follows moves the playback pointer to a session the
viewer is not watching.

**Acceptance:** `scripts/control-reporter-browser-check` exercises the path in
a real browser, and the PR quotes its output. The node suite alone does not
discharge this slice — §3's second paragraph is the reason.

---

## 6. Non-goals

§C13 applies in full. Web-specific additions:

- **Do not touch `clients/`.** Two other sessions are editing those trees right
  now.
- **Do not introduce a build step, a bundler, or an ES module.** The single
  embedded file is a deliberate constraint; a `type="module"` script in an
  `include_str!` page fails differently in every browser and silently in the
  test harness.
- **Do not relax `attachHls`'s teardown-first rule.** Add a path beside it. The
  comment at `index.html:6337` records the bug that rule fixed.
- **Do not edit the Settings > Developer copy** asserted at
  `tests/web/settings-sections.test.js:276`. It is currently true.
- **Do not use `localStorage` for preparation state.** It is per-viewer,
  per-browser, and survives a page the state must not survive.

---

## 7. Definition of done

1. `make web-check` green — all fourteen node suites, plus `scripts/js-check`
   and `scripts/contrast-check`. (`cluster-membership.test.js` at `Makefile:452`
   is separated from the rest by a comment and is the one people miss.)
2. `scripts/control-reporter-browser-check` run, output quoted in the PR.
3. `cargo test --workspace` green (the page is `include_str!`-embedded; a Rust
   test asserts page-read budgets).
4. §C12's eight shared acceptances all have a named test.
5. The PR description states plainly that **web's `dual_player_preparation`
   remains `false`, so this path is unreachable in production**, and links
   §C14 for why the work landed anyway.
6. `crates/plurxd/src/web/playback-control.js` still reads as the reference
   implementation the other two clients mirror — same function names, same
   ordering, same comment discipline.

---

## 8. Commands

```bash
make web-check                              # the local gate for any web change
node tests/playback/web-control.test.js     # the control-plane suite alone
node tests/web/settings-sections.test.js    # the copy tripwire
scripts/control-reporter-browser-check      # a real browser, a real exchange
cargo test --workspace                      # the page is embedded in the crate
```


---

# Part II — the wire contract, exactly as the server speaks it

Identical in the web, Apple and Android briefs, and maintained as a standalone
document at [`docs/M6-CLIENT-REPLACEMENT-CONTRACT.md`](M6-CLIENT-REPLACEMENT-CONTRACT.md).
**That copy is the one to edit** when the server changes; the three briefs are
snapshots taken at the baseline in this file's header. Everything below describes
shipped `main` behaviour, not proposed behaviour. Part I's `§C`-references
point here.

**Standing instruction.** Every type below was copied out of the tree at the
baseline in this file's header, with its line number. Re-verify each against
the cited file at build time in case it moved, and if a field's shape
disagrees with this document, **the code wins and this document is the bug** —
say so rather than reshaping the client to match prose.

**The second standing instruction.** Nothing in this contract is yours to
extend. If your platform seems to need a field, a state, or an action name
that is not written here, stop and flag it. All three clients are being built
at once against a server that is already frozen; a client that invents a sixth
acknowledgement state produces a `400` in the field and a merge conflict in
the protocol.

---

## C1. The situation, in one paragraph

**The server half is finished.** `ControlAction::Prepare` exists, staging
writes a durable row, the acknowledgement state machine is implemented and
fenced, commit and abort reach the store through a CAS, and a deadline timer
reaps a successor nobody claimed. All of it is covered by in-file tests. It is
nevertheless **completely dark in production**, for one reason: no shipped
client names `prepare_replacement` in `supported_actions`, so the server never
tells anyone a successor is staged. That single missing string is the whole
remaining gap, and closing it correctly is what these three briefs are for.

There is no `M6a` or `M6b` server milestone to wait for. There never was — the
letters do not appear anywhere in the repository. The slices that exist are
`M6-CALLER-HANDOFF.md` §3.1–§3.5, and all five are shipped.

---

## C2. Two gates, and they are independent

This is the most misread part of the system, so it is drawn rather than
narrated. A staged successor has to clear **both** gates before a client hears
about it, and they are checked at different times by different code.

```
 client's DynamicCapabilities                 client's supported_actions
   .dual_player_preparation                     contains "prepare_replacement"
            │                                              │
            │  GATE A — may the server                     │  GATE B — may the
            │  build a successor at all?                   │  server mention it?
            ▼                                              ▼
  decide_preparation_given_client              hls.rs:5648  can_settle_preparation
  playback_control.rs:1536                     hls.rs:5681  prepared_successor
            │                                              │
     false ─┴─▶ Fallback{ClientCannotPrepare}        false ─┴─▶ NotRequested
               nothing is staged                              staged row exists,
               nothing is spent                               client never told,
                                                              `suppressed` metric++,
                                                              reaped at 330 s
       true                                            true
            │                                              │
            ▼                                              ▼
      stage_prepared_successor                      ControlAction::Prepare
      hls.rs:6464 — real encoder,                   on the wire
      durable row, actor slot taken
```

**Gate A is a hardware claim.** It says *this platform can hold two live
decode pipelines*. M5.5 measured it on real devices and the answer is per
platform, frozen in protocol v1, and **not yours to change in a client brief**
(§C13).

**Gate B is a vocabulary claim.** It says *this build knows what to do with a
`prepare` action*. It costs nothing and is safe to declare the moment the
handling code exists, because a client that declares it and is never offered a
preparation behaves exactly as it does today.

The consequence that makes three parallel sessions possible: **all three
clients implement the same thing, and only Gate A decides whose path goes
live.** Apple's Gate A is already `true`, so Apple's work reaches viewers.
Web's and Android's stay `false`, so their work is proven by tests that inject
the action directly (§C12) and waits on a measurement, not on more code.

### C2.1 The one thing Gate B changes even when Gate A is false

Declaring `prepare_replacement` makes `can_settle_preparation` true
(`hls.rs:5648-5652`), which makes the server perform a quorum store read for a
staged generation on every exchange. On a client whose Gate A is `false`,
nothing is ever staged, so that read always returns `Absent` — correct, but not
free.

The cost is narrower than it first looks, because the predicate is a
disjunction:

```rust
let can_settle_preparation = request
    .accepts(PREPARE_REPLACEMENT_ACTION)
    || request.acknowledgement.is_some()
    || owner_epoch > 1
    || request.demand == PlaybackDemand::End;
```

A session already past its first owner epoch, or carrying an acknowledgement,
or ending, was doing the read anyway. Declaring the action adds it only for
sessions still on `owner_epoch == 1` with no acknowledgement and
`demand != end`. Say so in your PR; do not discover it in a dashboard.

---

## C3. The exchange, and where it lives

`POST /api/v1/hls/{session_id}/control` — registered at
`crates/plurxd/src/http/mod.rs:373-378` under the `/api/v1` prefix, handled by
`hls::control` at `crates/plurxd/src/http/hls.rs:4054`.

It is **poll-based**: the client POSTs its own state, the server answers with
at most one instruction. There is no push, no socket and no long poll. The
session UUID in the path **is** the bearer credential; there is no other auth
on this route.

**Cadence and bounds** (`crates/plurxd/src/playback_control.rs:21-62`):

| constant | value | meaning for a client |
|---|---|---|
| `PROTOCOL_V1` | `"plurx-playback-control-v1"` | echo verbatim in `protocol` |
| `NEXT_EXCHANGE_MS` | `5_000` | published as `next_exchange_ms`; the default pace |
| `MIN_CONTROL_INTERVAL` | `250 ms` | server-side floor; exceed it and you get `429` |
| `EXCHANGE_DEADLINE` | `4 s` | the server abandons its own work and answers `503` |
| `MAX_REQUEST_BYTES` | `16 KiB` | a larger body is refused before parsing |
| `MAX_RESPONSE_BYTES` | `64 KiB` | |
| `ROLLING_LEASE_TIMEOUT_MS` | `60_000` | one of exactly two legal `lease_timeout_ms` |
| `VOD_LEASE_TIMEOUT_MS` | `300_000` | the other |
| `TERMINAL_ACK_REPLAY_TTL_MS` | `60_000` | an accepted settlement replays for this long |
| `PREPARATION_DEADLINE_MS` | `330_000` | `VOD_LEASE_TIMEOUT_MS + 30_000`, `hls.rs:6446` |

**The client does not get to advertise itself into this route.** The control
block only appears in the session create response when the store setting
`PLAYBACK_CONTROL_PROTOCOL_V1` is `"1"` (`hls.rs:1854-1859`). A session created
with the gate off has no `control` block, and a control POST against it answers
`404 session_gone` with the message *"playback control was not advertised for
this session"*. This is how the whole protocol stays optional.

`ControlBootstrap`, what you receive in the create response
(`playback_control.rs:92-101`):

```rust
pub(crate) struct ControlBootstrap {
    pub protocol: String,
    pub url: String,             // "/api/v1/hls/{session_id}/control"
    pub generation: String,      // echo as `generation`
    pub control_epoch: u64,      // echo as `control_epoch`
    pub next_exchange_ms: u32,
    pub lease_timeout_ms: u32,   // exactly 60_000 or 300_000
}
```

---

## C4. The request

`ControlRequestV1` — `crates/plurxd/src/playback_control.rs:136-167`. There is
**no `rename_all`**: the Rust field names are already the wire names.
`#[serde(deny_unknown_fields)]` is on, so a key this server has never heard of
is a `400`, not a warning.

```rust
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlRequestV1 {
    pub protocol: String,
    pub generation: String,
    pub control_epoch: u64,
    pub client_instance_id: String,
    pub sequence: u64,
    pub demand: PlaybackDemand,
    pub position_ms: i64,
    pub buffered_from_ms: Option<i64>,
    pub buffered_through_ms: i64,
    pub playback_rate: f64,
    pub render_state: RenderState,
    pub seek_target_ms: Option<i64>,
    pub observed_download_bps: Option<u64>,
    pub selection: ClientSelection,
    pub capabilities: Option<DynamicCapabilities>,
    pub observation: Option<ClientObservation>,
    pub acknowledgement: Option<ActionAcknowledgement>,
    pub supported_actions: Option<Vec<String>>,
}
```

Two fields carry the whole of this milestone's client work: `supported_actions`
(§C5) and `acknowledgement` (§C7).

**`capabilities` is required on `sequence == 1`** and optional thereafter
(`playback_control.rs:245-249`). The server retains the last accepted document
on `ControlState::last_capabilities` (`:2907-2924`) and Gate A reads the
*retained* one, not this exchange's. So a client that sends capabilities once
and then omits them is behaving correctly and is still eligible to prepare.

**`supported_actions` bounds** (`:261-273`): at most `MAX_SUPPORTED_ACTIONS =
16` names, each non-empty and at most `MAX_ACTION_NAME_LEN = 32` bytes.
**Unrecognised names are ignored, never refused** — deliberately, so a client
from a later version can name actions this server has never heard of. Absent or
empty means *passive*, and a passive client is only ever answered `none`.

---

## C5. The name trap — read this twice

`prepare_replacement` and `prepare` are both correct, in different places, and
this is the single highest-risk item in the contract. Every other action's
declared name equals its wire tag; this one does not.

| where | string | source |
|---|---|---|
| what you **declare** in `supported_actions` | `"prepare_replacement"` | `PREPARE_REPLACEMENT_ACTION`, `playback_control.rs:53` |
| what arrives as `action.type` | `"prepare"` | `#[serde(tag = "type", rename_all = "snake_case")]` on the `ControlAction` enum, `:1609` |

The mapping is `ControlAction::vocabulary_name()` at
`playback_control.rs:1730-1739`. The comment there gives the reason: the action
is named for the transaction (`prepare_replacement` / `commit_replacement` /
`abort_replacement` in roadmap §3.1), while the tag is named for the moment.

A client that declares `"prepare"` is never offered anything — `accepts()` is a
literal string comparison (`:283-287`), it will not match, and the failure is
completely silent. A client that switches on `action.type == "prepare_replacement"`
never fires.

The server does assert the tag — `hls.rs:16375`,
`assert_eq!(prepare_body["action"]["type"], "prepare");`, against a serialized
body, and eight further sites read `["action"]["action_id"]` off one. **What
nothing asserts is the rest of the payload**: grep the tree for
`["action"]["playlist_url"]`, `["media_origin_ms"]` or `["effective_selection"]`
and you get nothing. So the tag is pinned and the four fields a client actually
consumes are not. Each client brief therefore requires a full literal-JSON
fixture of its own (§C12).

The full declared vocabulary after this milestone, on every platform:

```json
["hold", "retry_resource", "terminal", "prepare_replacement"]
```

Order is not significant — `accepts()` scans the list — but keep it in this
order across all three clients so the three test fixtures read the same.

---

## C6. `ControlAction`, in full

`crates/plurxd/src/playback_control.rs:1607-1685`. Internally tagged on `type`,
`rename_all = "snake_case"`. Note there is **no `deny_unknown_fields`** here:
a later server may add keys, and your parser must tolerate them.

| variant | `"type"` | payload | carries `action_id`? |
|---|---|---|---|
| `None` | `none` | — | no |
| `Hold` | `hold` | `reason` | no — advisory, recomputed on replay |
| `Terminal` | `terminal` | `code`, `message` | no |
| `RetryResource` | `retry_resource` | `after_ms`, `reason` | no |
| `Prepare` | `prepare` | see below | **yes** |

```rust
    Prepare {
        action_id: String,
        session_id: String,
        playlist_url: String,
        /// Exact source position the successor's session-relative zero maps
        /// to. Without it a client cannot align the second timeline with the
        /// first, and the commit boundary is expressed in film time.
        media_origin_ms: i64,
        /// What the successor will deliver. The client asked for a
        /// selection; this is the server's answer, and a client that no
        /// longer wants it can decline by simply not acknowledging.
        effective_selection: EffectiveSelection,
    },
```

**Field by field:**

- **`action_id`** — a UUID minted once per staging (`into_action()`,
  `:1712-1721`) and replayed byte-identically on every subsequent exchange until
  the staging settles. It is the transaction identity: you echo it in
  `acknowledgement.action_id`, and that is how a commit that arrives late names
  *this* staging rather than whichever one is current when it lands. Treat a
  repeat of the same `action_id` as *the same preparation*, not a new one.
- **`session_id`** — the successor's own session UUID. It is a full session in
  every respect but two: it holds no playback pointer, and it sits at
  `MEDIA_SESSION_PUBLICATION_BLOCKED`. You address it exactly as you would a
  session from a create response.
- **`playlist_url`** — node-relative, and validated to be exactly
  `/api/v1/hls/{session_id}/index.m3u8` or `…/master.m3u8`
  (`is_node_relative_playlist`, `:1759`; max 512 bytes; query and fragment
  allowed but ignored). Resolve it against the same origin as the current
  session. **Do not follow it as an absolute URL** even if a future server sends
  one — the relay path already refuses that (`a_relayed_preparation_cannot_point_a_client_anywhere`,
  `:13973`) and your client should too.
- **`media_origin_ms`** — the source position that the successor's
  session-relative zero maps to. This is the whole alignment mechanism; §C8.
- **`effective_selection`** — what the successor will actually deliver, not
  what you asked for. §C9. A client that no longer wants it declines by simply
  never acknowledging; the deadline reaps it.

**Ranking.** `resolve_action` (`:1929-1985`) returns a decided action verbatim
at `:1934-1936` and ranks it above every advisory one, so a `Prepare` outranks
a `Hold`. On the two *derived* branches — the producer-decision one at
`:1959-1966` and the hold one at `:1969-1970` — an action whose
`vocabulary_name()` you did not declare is replaced with `ControlAction::None`,
**never softened into a hold**. A decided action is vocabulary-resolved earlier,
at acceptance, so the client-facing rule is the same either way: **silence from
the server is not evidence that nothing is staged.**

**Exactly one action per exchange.** There is no array. A preparation and a
terminal cannot arrive together.

---

## C7. The acknowledgement

`ActionAcknowledgement` — `crates/plurxd/src/playback_control.rs:490-534`.
`deny_unknown_fields` is on.

```rust
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionAcknowledgement {
    pub action_id: String,
    pub state: AcknowledgementState,
    pub buffered_through_ms: Option<i64>,
    pub first_frame_unix_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcknowledgementState {
    MetadataReady,
    BufferReady,
    Committed,
    Failed,
    Aborted,
}
```

**Exactly five legal `state` strings**: `metadata_ready`, `buffer_ready`,
`committed`, `failed`, `aborted`. A sixth is a serde error, which means a
`400 invalid_control` with the generic message *"the control body is not valid
protocol v1 JSON"* and **no `invalid_field`** — it never reaches the semantic
validator, so you get no help identifying it. This is why §C1's second standing
instruction exists.

**Validation rules, all `400 invalid_control`:**

| rule | `invalid_field` |
|---|---|
| `action_id` must parse as a UUID | `acknowledgement.action_id` |
| `buffered_through_ms`, if present, ∈ `0..=MAX_MEDIA_MILLIS` | `acknowledgement.buffered_through_ms` |
| `first_frame_unix_ms`, if present, must be `> 0` | `acknowledgement.first_frame_unix_ms` |
| `buffer_ready` **requires** `buffered_through_ms` | `acknowledgement.buffered_through_ms` |
| `committed` **requires** `first_frame_unix_ms` | `acknowledgement.first_frame_unix_ms` |
| `demand == "end"` may not carry `state: "committed"` | `acknowledgement.state` |

That last one is worth a sentence of its own: **you may not commit a
replacement on the same exchange that ends the session**
(`playback_control.rs:253-259`). A client that switches players and then
immediately tears down on user-close must send the commit and the end as two
exchanges, in that order.

**The server's state machine** (`record_preparation_progress` `:3377-3410`,
`record_terminal_preparation_acknowledgement` `:3412-3444`):

| you send | server does |
|---|---|
| `metadata_ready` | records progress, if the prior ack is absent or `metadata_ready` |
| `buffer_ready` | records progress, if the prior ack is absent, `metadata_ready`, or `buffer_ready` — monotonic |
| `committed` | `PreparationDirective::Commit`; the store CAS runs; if the deadline or an abort already won, you get `Abort { acknowledgement_rejected: true }` |
| `failed` / `aborted` | `PreparationDirective::Abort { acknowledgement_rejected: false }` |

**An acknowledgement whose `action_id` does not match the currently bound
`Prepare` is silently ignored** (`bound_preparation_acknowledgement`,
`:3361-3375`) — *"Unknown action IDs belong to an older staging and are
deliberately harmless."* No error comes back. Do not treat a `200` as proof
your commit was accepted; read the response.

**Progress states are optional; a terminal state is not.** Nothing forces you
to send `metadata_ready` or `buffer_ready`, and the server behaves correctly
without them — they exist so the operator can see how far a preparation got
before it died. But a preparation you abandon **must** get `failed` or
`aborted`. Leaving it to the 330-second deadline holds the predecessor's
preparation slot for the rest of the session (`M6-CALLER-HANDOFF.md` §3.5), so
that session gets exactly one preparation, ever, good or not.

---

## C8. The protocol, end to end

```
 CLIENT                                                  SERVER
   │  POST control  { supported_actions: [... ,          │
   │                  "prepare_replacement"],            │
   │                  capabilities: { dual_player_       │
   │                    preparation: <measured> } }      │
   │ ──────────────────────────────────────────────────▶ │
   │                                                     │ viewer changes quality,
   │                                                     │ or the actor proposes one
   │                                                     │ decide_preparation → Prepare
   │                                                     │ stage_prepared_successor:
   │                                                     │   mint incarnation + session,
   │                                                     │   start a real encoder,
   │                                                     │   durable row, pointer UNTOUCHED
   │                                                     │ arm 330 s deadline
   │  ◀────────────────────────────────────────────────  │
   │   action { type:"prepare", action_id, session_id,   │
   │            playlist_url, media_origin_ms,           │
   │            effective_selection }                    │
   │                                                     │
   ├─ build SECOND pipeline on playlist_url              │
   ├─ align its zero to media_origin_ms  ────────────┐   │
   │                                                 │   │
   │  POST { acknowledgement: {action_id,            │   │
   │          state:"metadata_ready"} }              │   │
   │ ──────────────────────────────────────────────────▶ │ progress recorded
   │                                                 │   │
   ├─ prime the successor's buffer                   │   │
   │  POST { acknowledgement: {action_id,            │   │
   │          state:"buffer_ready",                  │   │
   │          buffered_through_ms} }                 │   │
   │ ──────────────────────────────────────────────────▶ │ progress recorded
   │                                                 │   │
   ├─ SWITCH: successor visible, predecessor retired │   │
   ├─ note the first qualifying frame's wall clock ◀─┘   │
   │  POST { acknowledgement: {action_id,                │
   │          state:"committed",                         │
   │          first_frame_unix_ms} }                     │
   │ ──────────────────────────────────────────────────▶ │ Commit → store CAS from the
   │                                                     │ expected predecessor to the
   │                                                     │ staged incarnation
   │  ◀────────────────────────────────────────────────  │
   │   action { type:"none" }   ← there is no            │ predecessor state != active,
   │                              commit_replacement     │ ledger cleared, slot freed
   │                              action; §C11
   ▼
```

**The unhappy paths, all of which you must implement:**

```
 successor will not build   ──▶  ack {state:"failed"}     ──▶ Abort, slot freed
 viewer seeks / changes     ──▶  ack {state:"aborted"}    ──▶ Abort, slot freed
   quality again
 you never answer           ──▶  330 s deadline reaps it  ──▶ slot freed by the actor,
                                                              your late commit gets
                                                              acknowledgement_rejected
 commit lands after abort   ──▶  Abort{acknowledgement_rejected:true}
```

---

## C9. `effective_selection`

`crates/plurxd/src/playback_control.rs:861-891`. It appears **twice on the
wire**: as `ControlResponseV1.effective_selection` (what the *current* session
delivers) and inside `Prepare` (what the *successor* will deliver). Comparing
the two is how a client knows what is about to change.

```rust
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectiveSelection {
    pub quality_auto: bool,
    pub height: i64,
    pub audio_track: Option<i64>,
    pub subtitle_burn: Option<i64>,
    pub audio_offset_ms: i64,
    pub codec: String,
    pub dynamic_range: Option<String>,
}
```

**`codec` is not a codec name.** Legal values are exactly `"source"` and
`"server_selected"`, and they distinguish direct-play/remux from transcode —
the `PreparationAxis::DeliveryMethod` axis. A client that renders this string
to a viewer as a codec is wrong.

`dynamic_range` ∈ `{"dolby_vision", "hdr10", "hlg", "sdr"}`, or absent/null.
`height` ∈ `0..=MAX_HEIGHT`. `audio_offset_ms` ∈ `-15_000..=15_000`.
`audio_track` and `subtitle_burn` ∈ `0..=1_024` when present.

On the response path the height comes from the stored `StartResponse` and the
grade from `start.delivered_dynamic_range` (`hls.rs:4183-4187`) — **the
encoder's answer, not the request's**.

### C9.1 Which changes can ever be prepared

`PREPARED_AXIS_SETS` — `playback_control.rs:1376-1381` — admits exactly two:

```rust
const PREPARED_AXIS_SETS: [PreparationAxisSet; 2] = [
    PreparationAxisSet::of(&[PreparationAxis::ResolutionOrBitrate]),
    PreparationAxisSet::of(&[
        PreparationAxis::ResolutionOrBitrate,
        PreparationAxis::DeliveryMethod,
    ]),
];
```

plus a directionality guard (`successor_rate_is_bounded`, `:1408-1410`): the
two-axis pair is admitted **only toward a `server_selected` successor**.

Both admitted sets are **same-codec**. A codec or dynamic-range change is never
prepared today, on any platform. This matters when you read the M5.5 numbers
(§C13) because the case web failed is not a case the server would have offered
it.

`FallbackReason` strings a client may see in metrics, never on the wire
(`:1091-1103`): `client_cannot_prepare`, `axis_not_proven`, `multiple_axes`,
`throughput_unreported`, `throughput_insufficient`.

### C9.2 The throughput floor, and why `observed_download_bps` is load-bearing

`PreparationConditions::headroom_refusal` (`:1173-1194`) requires **both**
`observed_download_bps` (yours) and `delivered_bps` (the server's) to be
present, `delivered > 0`, and `observed >= 2 * delivered`. Either missing gives
`ThroughputUnreported` and no preparation.

**A client that sends `observed_download_bps: null` can never be offered a
preparation, whatever its capability says.** Dual preparation doubles network
demand, and M5.5 never tested a contended link
(`M6-IMPLEMENTATION-HANDOFF.md` §8) — the floor is the only thing standing in
for that measurement. If your platform currently sends null, fixing that is
part of your brief, not an optimisation.

**And the half you cannot fix from a client: `delivered_bps` is `None` on every
VOD session.** The `FallbackReason::ThroughputUnreported` doc says it in as
many words (`playback_control.rs:1074-1082`): *"`DeliveryView::from_status`
leaves `delivered_bps` `None` on every VOD session, which is most of them."*
Since the floor needs both numbers, **a prepared handoff cannot fire on a VOD
session today, on any platform, however capable the client is.** Only live and
live-recovery sessions can reach it.

Three consequences worth carrying into every plan:

- The `ThroughputUnreported` counter is currently measuring its own missing
  inputs, not tight links. Do not read it as evidence about networks.
- **A hardware acceptance must be taken on a live session.** A tester who
  changes quality on a VOD title and sees no preparation has reproduced this
  gap, not a client bug, and will spend a day on it.
- The same doc comment names this as *"the residual worth re-reading if
  prepared handoffs turn out never to fire"*. If yours never fires, start here
  before you start in your own code.

Closing the VOD half is server work, out of scope for all three client briefs.
File it; do not fix it in a client PR.

---

## C10. Terminal reasons — 15 values, 3 of them permanent

`ProducerDecisionReason` — `playback_control.rs:3847+`. These strings appear in
`delivery.producer_decision`, in `Terminal.code`, and in `RetryResource.reason`.

```
startup_deadline · progress_deadline · exit_classification_deadline
process_exit · partial_success_exit · unsupported · invalid_configuration
reader_failed · flow_stop_failed · flow_resume_failed · flow_stop_deadline
flow_resume_deadline · install_deadline · executor_lost · source_decode_failed
```

`is_permanent()` is exactly `{unsupported, invalid_configuration,
source_decode_failed}`. A permanent reason becomes `Terminal`; everything else
becomes `RetryResource`. **`source_decode_failed` is new in this effort** and
is the reason the decoder work exists: the process may exit zero and still have
decoded nothing, and every timing reason tells a client to try again, which
reproduces the fault.

Two cautions:

1. `terminal_message()` (`:1992-2007`) has no prose sentence for
   `source_decode_failed` yet, so its `message` on the wire is currently the
   literal string `"source_decode_failed"`. **Do not render `message` to a
   viewer unmodified.** Map `code` to your own localised copy.
2. There is a **second, unrelated** terminal vocabulary for session teardown in
   `crates/plurxd/src/vodserve.rs:141-192` (`deleted`, `superseded`,
   `admin_stop`, `revoked`, `replaced`), delivered on `410` answers. It is not
   `ProducerDecisionReason` and must not share a switch statement with it.

---

## C11. There is no `commit_replacement` action

Roadmap §3.1 names three transactions — `prepare_replacement`,
`commit_replacement`, `abort_replacement` — and only the first is an action.
Commit and abort are expressed as **client acknowledgement states**, and the
server's reply on the settling exchange is `{"type":"none"}`
(asserted at `hls.rs:15547`, and again at `:16402` and `:16719`).

So: the client decides when to switch. The server does not order the switch and
cannot; it only stages a successor, waits, and settles what it is told. Any
design that waits for a `commit_replacement` action will wait forever.

---

## C12. Acceptance shared by all three clients

Every platform's brief inherits these. Each is a runnable test or an observable
fact.

1. **The declared vocabulary is exactly
   `["hold","retry_resource","terminal","prepare_replacement"]`**, asserted
   against the serialized request body, not against the in-memory list. The
   serializer is what ships.
2. **A literal-JSON fixture** for the inbound action, asserted key by key:
   ```json
   {"type":"prepare",
    "action_id":"…uuid…",
    "session_id":"…uuid…",
    "playlist_url":"/api/v1/hls/…/index.m3u8",
    "media_origin_ms":0,
    "effective_selection":{"quality_auto":true,"height":1080,
      "audio_track":null,"subtitle_burn":null,"audio_offset_ms":0,
      "codec":"server_selected","dynamic_range":"sdr"}}
   ```
   This exists in **no** test in the repository today. Write it before you write
   the handler; it is the assertion that catches the `prepare` /
   `prepare_replacement` trap.
3. **An unknown `action.type` is still fatal** — the existing behaviour on all
   three clients — and a test proves the new vocabulary did not weaken it.
4. **A `prepare` with a `playlist_url` that is not node-relative is refused**
   by the client, without a network request. Mirror
   `a_relayed_preparation_cannot_point_a_client_anywhere`.
5. **The three terminal acknowledgements round-trip**: `committed` carries
   `first_frame_unix_ms`, `buffer_ready` carries `buffered_through_ms`, and a
   `committed` on a `demand: "end"` exchange is never constructed.
6. **A preparation the client abandons sends `failed` or `aborted`** — proven
   by a test that drives the abandon path and asserts the outbound body, not by
   inspection.
7. **A repeated `action_id` is one preparation.** Feed the same action twice
   and assert exactly one pipeline is built.
8. **`observed_download_bps` is populated** on every exchange where the
   platform can measure it, asserted on the serialized body.

---

## C13. Non-goals — guardrails, each with its reason

- **Do not change `dual_player_preparation` in a client brief.** It is a frozen
  v1 field whose value was measured on hardware
  (`M6-IMPLEMENTATION-HANDOFF.md` §1), and three literals were already written
  by assumption once. Changing it authorises the server to prime a second
  pipeline on a viewer's device. If your measurement disagrees with the table
  in §C14, that is a finding to file, not a literal to edit.
- **Do not add a row to `PREPARED_AXIS_SETS`.** It needs a hardware receipt run
  above the throughput floor. The table's own doc carries the reason.
- **Do not call any prepared path "seamless"** in UI copy, in comments, or in a
  PR description. Plan §5.2 draws the line and M5.5 gave it numbers: web/Safari
  fallback interruption 271–2,246 ms (mean 1,121), Android/Google TV
  353–766 ms (mean 471). Apple's fallback is **unmeasured**, because Apple
  passed dual preparation and never exercised it.
- **Do not describe a finite retained buffer as a running predecessor.**
- **Do not delete the client-side recovery budgets.** They bound the *count* of
  reconnections; `retry_resource { after_ms }` bounds only the *rate*, and
  nothing in the protocol bounds the count (`M6-IMPLEMENTATION-HANDOFF.md` §2,
  Q3). M5c and M5h were struck for exactly this reason.
- **Do not invent a sixth acknowledgement state, a second action name, or an
  extra request field.** §C1.
- **Do not make the second pipeline audible or visible before the switch.** The
  predecessor is the authoritative stream until the moment you commit.

---

## C14. What is measured, what is assumed, and what is open

**Measured on hardware, 2026-09-01 to 2026-09-03**
(`PLAYBACK-CONTROL-STATUS.md:1214-1218`, `M6-AXIS-CASE-HANDOFF.md`):

| platform | `dual_player_preparation` | evidence |
|---|---|---|
| Apple | **`true`** | iPhone 17 Pro Max and Apple TV 4K, 20/20 on both cases, zero stalls, after a first instrument set was rejected and repaired |
| web | `false` | Safari **same-codec reached 20 consecutive**; codec/HDR only 13/20 |
| Android | `false` platform-wide | both phones passed both cases; the tunneled Google TV passed codec/HDR and **failed same-codec 0/3** |

**The finding this document is obliged to record, because it changes what two
of these three sessions are building toward.** The only axes the server will
ever prepare are same-codec (§C9.1). Line the two tables up:

- **Web's `false` is protecting against a case the server cannot offer it.**
  Safari's failure was codec/HDR; its same-codec result was 20/20 — the exact
  axis `PREPARED_AXIS_SETS` admits.
- **Android's `false` is correct for the television class and wrong for
  phones.** The Google TV's only hard failure was on same-codec, which *is* the
  common case; the phones passed both.

Neither observation authorises editing a literal, because the field is
platform-wide and frozen: a `true` on web is also a `true` on every browser
that is not Safari, and a `true` on Android is also a `true` on the Google TV
that failed. **The fix is a narrower capability keyed by axis and device
class, which is a protocol v2 decision, not a client one**
(`M6-IMPLEMENTATION-HANDOFF.md` §3, `PLAYBACK-CONTROL-STATUS.md:1221-1227`).
Recording it here so that three sessions building the client half do not each
rediscover it and each reach a different conclusion.

**Assumed, and recorded as an assumption** (no user decision was available):
all three clients implement the full replacement path now, and Gate A decides
whose path is live. The alternative — build Apple only — was rejected because
it leaves two platforms with no tested code path on the day a narrower
capability lands, which is precisely when the measurement pressure will be
highest.

**Open, and not answerable from this document:**

- How long a client may wait for an action before falling back (roadmap §8 Q1).
- Whether `terminal` ends playback or offers a *Try again* (Q2). This is a
  product decision, and the three clients currently differ.
- Twenty consecutive commits on a realistic runway — nobody has them; M5.5's
  20/20 ran on a fixture short enough to buffer completely.
- Apple decoder memory across repeated trials: AVFoundation decodes in
  `mediaserverd` and the tooling exposes no resident memory for
  `mediaplaybackd` / `videocodecd`, so the one measurement that would close the
  runway residual is the one that cannot currently be taken.
- Behaviour under an active throughput drop. The whole spike ran on a steady
  shaped 80 Mbit/s link. The throughput floor (§C9.2) stands in for this and has
  not been validated against a real contended link.
