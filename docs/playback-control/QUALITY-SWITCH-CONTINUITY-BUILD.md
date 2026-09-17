# Quality switch continuity — build plan for the executing agent

**Status:** revised after review, awaiting re-review · **Executes:** the
rulings in [QUALITY-SWITCH-CONTINUITY-PLAN.md](QUALITY-SWITCH-CONTINUITY-PLAN.md)
§9, taken 2026-09-16 · **Review:** Astra, 2026-09-16, changes requested
(R1–R8 + workflow); every finding is dispositioned in §11 · **Base:** `main`
at `c9e4edf4` or later · **Written:** 2026-09-16, revised the same day ·
**Order:** M0 → M2 (Apple, Android) → M1 (web) → M3, on one effort branch.

Read [the plan](QUALITY-SWITCH-CONTINUITY-PLAN.md) §1–§2 first; it says what
the tree does today and why no viewer has ever received a prepared quality
handoff, with anchors. This document says what to change, file by file, and
how each change is proven. Work milestone by milestone, one pull request each,
in the order above. **If a step seems to require changing something this
document does not name — the wire shape of `Prepare`, the acknowledgement
states, the 330 s deadline, the reporter's pinned `renew_after_ms`, the
`hold` semantics — stop and flag it instead of improvising.** Every line
number below is from `c9e4edf4`; re-verify against the file before editing.

## 1. Objective and the bar

A viewer-directed quality change (the quality menu, on any client) reaches the
new rendition through the M6 prepared handoff, with the incumbent playing —
picture and sound — until the successor's first frame is on screen. Every
directed change ends in exactly one of two ways: a committed successor, or
**one** ordinary reopen at the film position the viewer has actually reached
by then. Never neither (the viewer left on the old quality with nothing
pending), never both (the stream changed twice for one tap).

The two time bounds, stated once (this settles R7):

- **The offer bound — 12 s from the tap (D1).** If no `Prepare` for the ask
  has arrived 12 s after the viewer's tap — whether because no exchange was
  accepted, the server answered `staging` throughout, or the server answered
  `none` — the client reopens. The clock starts at the tap, not at the
  accepted exchange, so a slow first exchange cannot stretch it.
- **After an offer, the existing M6 bounds govern**: readiness (Apple
  `readinessMs = 12_000`, `PreparedReplacement.swift:318`; Android
  `PREPARED_READINESS_BOUND_MS = 20_000`, `PreparedReplacement.kt:24`; web
  none of its own — the server's deadline) and first frame (Apple 6 s, Android
  5 s, web 8 s). Their failure now ends in the one reopen (§5.3, §6.3, §7.3),
  which today it does not on web and Android. The worst case for a directed
  change is therefore offer bound + readiness + first frame, and it is
  bounded; the plan's §3 "several seconds" and its earlier "no later than
  12 s" are replaced by this paragraph.

The bar (M3 measures it): twenty consecutive directed changes per platform,
realistic runway, fleet build, **zero dropped frames** in the two seconds
around the commit and **no audible seam**. Tap-to-new-quality wall time is
reported, not judged.

## 2. Rulings already taken — do not reopen them

| Ruling | Answer | Where it lands |
|---|---|---|
| D1 wait bound | **12 s from the tap** to the offer; M6's bounds after (§1) | §5.1, §6.1, §7.1 |
| D2 how the server says "building" | **A field**, `delivery.preparation` ∈ `staging` · `offered` · `none`, optional, absent on older relays | §4.1 |
| D3 Auto | **D3-a now**: the web Auto controller sends the rung it wants as `quality: {mode:"auto", height}`; server-proposed Auto is a later plan | §7.4 |
| D4 Apple switch primitive | **Keep `replaceCurrentItem`**; M3 measures it | §5.4 |
| D5 order | **M0 → M2 → M1 → M3** | this document |

## 3. How to work

- **Branch model: one effort branch, `effort/quality-switch-continuity`.**
  [AGENTS.md](../../AGENTS.md) §Large efforts requires it for a multi-task
  project unless the plan proves no two tasks touch the same file; M0 and M1
  both edit `playback_control.rs` and `http/hls.rs`, so the exception does not
  apply. Each milestone is a task branch off the effort, PR'd back into it,
  gated by `Effort development gate`; when M3's results exist, merge current
  `main` into the effort and open the effort into `main` under `Main
  promotion gate`. The ownership table below is still binding *within* the
  effort so Apple and Android can run in parallel.
- **PR lifecycle** (Paul's, 2026-09-07): proper commits · fast lane only until
  the PR is together · open as `WIP:` · one adversarial review of the whole
  PR · implement the findings · full suite once · fix to green · un-WIP
  (`PATCH /pulls/<n>` title) · merge it yourself.
- **Compile before you push.** Rust: [docs/ci/AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md);
  the lab build host is nuc3 (`ssh pjunod@192.168.4.7`, rustup 1.97.1 at
  `~/.cargo/bin`, clone under `~/work/<name>`, `CARGO_TARGET_DIR` may point at
  an existing warm target). Apple: `make apple-test` on the macOS runner
  `pjunod@192.168.5.115` (`export PATH=/usr/local/bin:/opt/homebrew/bin:$PATH
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`; ~150 s warm).
  Android: `./gradlew --no-daemon :app:testDebugUnitTest :app:assembleDebug
  :app:lintDebug` with `platforms;android-37.0` from `--channel=3`.
- **Commit subjects.** `validation/history.py`'s `ISSUE_RE` treats `fix`,
  `stop`, `avoid`, `correct`, `prevent`, `remove`, `restore`, `keep`, `bound`,
  `stale`, `wrong`, `broken`, `failure`, `regression`, `fallback`, `truth` as
  corrective. A corrective commit touching `crates/` needs a
  `validation/regressions.d/<lead-sha>-<slug>.toml` row; one touching
  `clients/` with a `fix(` subject needs a `tests/client-fixes.toml` row. M0's
  cancellation *is* corrective (§4.2) — budget the row. Everything else: write
  `feat(`/`test(`/`docs(` subjects that do not match the regex.
- **Shipped-bytes bumps.** Any non-comment change under
  `clients/apple/Sources/**` needs `CURRENT_PROJECT_VERSION` in
  `clients/apple/project.yml` (165 at base) bumped, and any under
  `clients/android/app/src/main/**` needs `versionCode` in
  `clients/android/app/build.gradle.kts` (103 at base) bumped;
  `validation/mobile_versions.py` fails the build otherwise and
  `doc_versions.py` mirrors the numbers into `clients/apple/README.md`,
  `docs/clients/APPLE-CLIENT-PARITY.md` and `docs/STATUS.html`.
- **Owners census.** `tests/validation/test_rolling_producer_ownership_inventory.py`
  trips on every new `::spawn(` and `.status(` in the producer-owner files and
  on reads of `publication_ready_at_ms` / `MEDIA_SESSION_*`. Run it on the
  branch you are about to push and add rows to
  `tests/playback/rolling-producer-owners.toml` for anything it names.
- **Status page.** Update the M6 row and the "M6 clients and server
  reserve-and-prime" section of
  [PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) in each milestone's
  PR; it currently claims Apple's `canOfferPreparation` exists (it does not,
  since `PreparedReplacement.swift:423` hard-codes `canPrepare: true`).

### 3.1 File ownership within the effort

| Milestone | Owns | Must not touch |
|---|---|---|
| M0 server | `crates/plurxd/src/playback_control.rs`, `crates/plurxd/src/http/hls.rs`, `crates/plurxd/src/media_sessions.rs` (only if a cancel hook needs it), `tests/validation/test_control_wire_conformance.py`, `tests/playback/rolling-producer-owners.toml`, `validation/regressions.d/*` | any `clients/**` |
| M2 Apple | `clients/apple/Sources/PlayerController.swift`, `PlaybackControlSession.swift`, `PlaybackControlReporter.swift`, `PreparedReplacement.swift`, `LiveTvDeveloperView.swift`, `clients/apple/Tests/**`, `clients/apple/project.yml` | server, Android, web |
| M2 Android | `clients/android/app/src/main/java/tv/plurx/app/player/{Controller,PreparedReplacement,PlaybackControlReporter,PlaybackControlSession,PlaybackIntent}.kt`, `ui/SettingsScreen.kt`, `clients/android/app/src/test/**`, `app/build.gradle.kts` | server, Apple, web |
| M1 web | `crates/plurxd/src/web/index.html`, `crates/plurxd/src/web/playback-control.js`, `tests/playback/web-control.test.js`; **after M0 merges into the effort** also `playback_control.rs`, `http/hls.rs` and `crates/plurx-core/src/playback/desired.rs` for §7.4 | Apple, Android |
| M3 | per-client instrumentation files above, `tests/playback/playback-info-fields.json`, docs | protocol types |

Apple and Android M2 may run as two PRs in parallel; they share nothing.

## 4. M0 — the server

Two changes and their proof. PR title: `feat(playback): say when a successor
is being prepared, and stop preparing one nobody will watch`. The second
clause makes the commit corrective; split it into two commits so only the
cancellation commit needs the regressions row.

### 4.1 `delivery.preparation`

**Type.** In `crates/plurxd/src/playback_control.rs`, `DeliveryView`
(`:898`) gains, after `subtitle_readiness`:

```rust
/// Whether this playback's one preparation slot is doing anything, when
/// this server evaluated it.
///
/// `staging` — a successor is being planned, reserved or primed for the
/// current ask and no `Prepare` has been announced yet; the client should
/// keep the incumbent playing and exchange again soon. `offered` — this
/// response's `action` is that `Prepare`. `none` — nothing is being built
/// for the current ask, so a client waiting for a handoff should stop
/// waiting. Absent means "not evaluated here" (an older relay peer), never
/// `none`: a client must not read absence as a decline.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub preparation: Option<String>,
```

Bound it the way `hold_reason` is bounded: `DeliveryView`'s validation
already has `hold_reason.as_deref().is_none_or(|value| { … })` with the
literal vocabulary inline, and `test_the_hold_reason_is_one_vocabulary`
(`tests/validation/test_control_wire_conformance.py:435`) regex-reads that
block. Add the sibling `preparation.as_deref().is_none_or(|value| {
matches!(value, "staging" | "offered" | "none") })` in the same function so
the new conformance test (§4.3) can read it the same way.

**Ownership from dispatch to registration (this settles R1).** Today the
work between the dispatch exchange and `register_active_preparation` is
invisible: `process_preparation_candidate` reads the setting, the source row
and runs `plan_preparation_candidate` (`http/hls.rs:8082-8121`) — several
store awaits — before `stage_prepared_successor_with_prime` registers the
`ActivePreparedSuccessor` inside its `prime_worker` branch (`:8608`). An
exchange landing during those reads has neither a new dispatch nor a registry
entry, and a naive rule would answer `none` — which the client reads as a
decline and reopens on. So the field is derived from a **pending-candidate
marker** that spans the whole detached task:

```rust
/// One entry per playback whose ask is being turned into a successor, from
/// the moment the exchange spawns the candidate until that task exits by
/// any path — refused, unplannable, disabled, cancelled, or registered as
/// an `ActivePreparedSuccessor` (at which point the registry takes over).
/// Keyed by playback id; the value is the desired digest the candidate is
/// for, so a superseding ask is visible as "pending for a different ask".
static PENDING_PREPARATION_CANDIDATES:
    Mutex<HashMap<String /* playback_id */, String /* desired digest */>>;

/// RAII guard: inserted by the exchange handler immediately before
/// `tokio::spawn(process_preparation_candidate(..))` (`hls.rs:7212`), moved
/// into the task, removed on drop. A guard, not a call at each `return`,
/// because the task has nine early exits today and will grow more.
struct PendingCandidate { playback_id: String }
impl Drop for PendingCandidate { fn drop(&mut self) { /* remove */ } }
```

The registry hand-over is explicit: `register_active_preparation` is called
while the guard is still alive, so there is no instant at which neither
names the playback.

**Emit rule.** `local_control_response` (`http/hls.rs:4657`) builds the
response with `action: None`; the caller resolves the action afterwards
(`:4699` onward, the `Prepare` observation at `:6710-6728`) and computes
`preparation_purpose` at `:7172-7183`. Set the field once both are known,
immediately before serialization:

```rust
response.delivery.preparation = Some(
    if matches!(response.action, ControlAction::Prepare { .. }) {
        "offered"
    } else if preparation_purpose.is_some()               // this exchange dispatched
        || pending_candidate_for_playback(&route.playback_id).is_some()
        || has_active_preparation_for_playback(&route.playback_id)
    {
        "staging"
    } else {
        "none"
    }
    .to_owned(),
);
```

`has_active_preparation_for_playback` is a read-only sibling of
`take_active_preparations_for_playback` (`:7346`). Re-verify at build time
that `preparation_purpose` is computed before the response is serialized; if
the handler's order makes that impossible without moving the spawn, compute
the purpose earlier and pass it down — do not move the spawn itself, its
"spawned, never awaited" placement is load-bearing (`:7210-7212`).

Edges a test pins: `demand: end` answers `none` (the purpose is `None` and
End aborts the slot, `:6718-6721`); a replayed exchange whose staging has
since been cancelled answers `none`, because both marker and registry entry
are gone; an exchange that arrives while `plan_preparation_candidate` is
still awaiting answers `staging` (R1's case); a candidate that exits
unplannable flips the next exchange to `none`.

**Relay.** `relay_if_remote` forwards the owner's response body; confirm the
field survives the relay's re-validation (`ControlResponseV1::is_valid_for`
is the only check) and add the field to the relay's own conformance fixture
if one exists under `tests/playback/`.

### 4.2 Supersession cancels the preparation

**The defect.** The cancellation edges are commit (`:5034`), incumbent
`Waiting|Stalled` (`cancel_preparations_for_incumbent_wait`, `:7404`),
foreground contention (`:7307-7329`), settings disable (`:7410`, `:7425`) and
the 330 s deadline (`:8745`). A predecessor that is *superseded* — the client
reopened — cancels nothing, so its speculative worker runs on for up to 330 s
or is killed by the reopen's own admission pressure and counted as `refused`.

**The change.** One helper beside `cancel_preparations_for_incumbent_wait`:

```rust
/// The client went around the handoff — it created a new session for this
/// playback instead of committing the one being prepared — so the
/// successor has no viewer. Free the worker now, not at the deadline, and
/// drop a pending candidate that has not reached the registry yet.
fn cancel_preparations_for_superseded_predecessor(playback_id: &str, keep: Option<&str>) {
    cancel_pending_candidate(playback_id);            // §4.1's marker; the task observes it and exits
    for active in take_active_preparations_for_playback(playback_id) {
        if Some(active.preparation.incarnation_id.as_str()) == keep { re_register(active); continue; }
        spawn_cancelled_preparation(active, "predecessor superseded by a new session");
    }
}
```

`keep` is the activating route's `incarnation_id`: the one successor that
must survive its own predecessor's retirement is the committed one. Commit
already removes the registry entry at `:5034`, so today the guard is never
the only thing standing between a committed successor and cancellation — but
the guard is what makes that structural rather than an ordering accident,
and §4.3 tests it with an entry present.

Call it from **both** places a predecessor is superseded:

1. `settle_activation_predecessor` (`:3469-3531`) — the reopen-with-
   `previous_session_id` path, after the predecessor is projected and before
   the handoff completes, with `keep = Some(activating.incarnation_id)`.
2. `create_with_purpose` (`:1687`) — a plain create for a `playback_id` that
   has a pending candidate or an active preparation and no
   `previous_session_id` (a client that reopened without naming its
   predecessor). `keep = None`: a POST create is never the staged successor,
   which is minted by staging, not by a client.

Do **not** add it to `end_media_session` for reason `"superseded"` at `:5757`
or `:6565` — those are the *successor's* commit and `Switched` retiring the
predecessor, and the registry entry is already gone; a call there would be a
no-op that reads as a cancellation edge to the next reader.

`spawn_cancelled_preparation` already runs `settle_cancelled_preparation`
(`:7361`), which aborts or discards the durable row, then
`retire_prepared_worker` — the same path the incumbent-wait cancel takes, so
no new teardown code. For the pending case, `process_preparation_candidate`
checks its marker at each await boundary after the reservation (`:8610`) and
exits through the existing `record_preparation_staged(false)` path when the
marker is gone.

**Attribution** (Paul's standing rule: anything that uses the GPU is
attributable from inside the product). Add
`plurx_playback_preparation_cancelled_total{reason}` next to
`plurx_playback_preparation_staged_total` (`playback_control.rs:13790`), with
`reason` ∈ `predecessor_superseded` · `incumbent_waiting` ·
`foreground_claimed` · `disabled` · `expired` · `planned_outage`, recorded
inside `settle_cancelled_preparation` from the `&'static str` it already
receives (map the existing strings to these labels; do not change the
strings, tracing filters name them). Confirm the speculative successor
appears on the Activity page with a purpose while it is priming — if it does
not, add the purpose (`prepared_quality_handoff`, the plan note at
`http/hls.rs:8499`) to the activity row in this milestone; a viewer must be
able to see why the encoder is busy.

### 4.3 Proof for M0 (this settles R8)

The stage-only path (`stage_prepared_successor`, `#[cfg(test)]`, `:8287`)
does **not** register an `ActivePreparedSuccessor`; registration is inside
the `prime_worker` branch. Tests of cancellation therefore need a way to
register an entry without a real encoder. Add a `#[cfg(test)]` constructor,
`register_test_preparation(state, preparation, purpose) -> CancellationToken`,
that builds an `ActivePreparedSuccessor` around a `PreparationExecutor` whose
store is the test store and whose worker retirement is a no-op, and inserts
it exactly as `register_active_preparation` does. Every cancellation test
below uses it; none uses the stage-only path to stand in for registration.

Unit tests in `http/hls.rs`'s test module, next to the existing staged
successor tests:

- **`delivery_preparation_says_staging_on_the_exchange_that_dispatched`** —
  a control exchange with a changed selection answers `preparation ==
  "staging"` and `action == None` in the same response.
- **`delivery_preparation_stays_staging_while_the_candidate_is_planning`** —
  inject a `plan_preparation_candidate` delay longer than one client poll
  (use the existing test fault hooks, cf. `staged_read_faults()` at `:6699`),
  drive two more exchanges during it, assert both answer `staging`; then let
  the plan fail and assert the next exchange answers `none`. Also: supersede
  the predecessor during the delay and assert the task exits without staging
  (`record_preparation_staged(false)`) and the marker is gone.
- **`delivery_preparation_says_offered_with_the_prepare_action`** and
  **`…_says_none_when_the_slot_is_empty`**, plus `demand: end` → `none`.
- **`superseding_the_predecessor_cancels_its_registered_preparation`** —
  register with the test constructor, then activate a new session with
  `previous_session_id` = predecessor; assert the registry entry is gone,
  the durable row is aborted/discarded, the settlement ran with the new
  reason, and `cancelled_total{reason="predecessor_superseded"}`
  incremented.
- **`the_activating_successor_is_kept_by_the_supersession_cancel`** — the
  guard test, **with an entry present**: register an entry whose
  `incarnation_id` equals the activating route's, run the supersession
  cancel with `keep = Some(that id)`, assert the entry survives and no
  settlement ran. Then remove the `keep` comparison and confirm this test
  fails. This is the mutation that proves the guard; the post-commit test
  below cannot, and is kept as a lifecycle check only.
- **`a_committed_successor_survives_the_predecessors_retirement`** — commit
  through the real path, then let the predecessor end; the successor's route
  stays active. Lifecycle check; not a guard mutation.
- **`a_plain_create_for_the_playback_cancels_the_preparation`** for the
  second call site, with a registered entry and again with only a pending
  marker.

Cross-port gate: extend `tests/validation/test_control_wire_conformance.py`
with `test_the_preparation_state_is_one_vocabulary`, modelled on the
`hold_reason` test, reading the `is_none_or` block from the Rust and
requiring any port whose `ControlDelivery` declares `preparation` to compare
against the same three strings. In M0 only the Rust declares it; the test
must pass with zero, one, two or three client ports declaring it (the same
shape `test_the_acknowledgement` already has for `ActionAcknowledgement`).

Commands: `make validate-staged` per commit; `cargo test -p plurxd
preparation` for the focused set; `python3 -m unittest
tests.validation.test_control_wire_conformance
tests.validation.test_rolling_producer_ownership_inventory`; the full
`make ci-rust-gate` once after review. Acceptance on the fleet after Paul
deploys: `curl -s http://<node>:32400/metrics | grep
plurx_playback_preparation_` — a directed change from a current client now
increments `cancelled_total{reason="predecessor_superseded"}` (the clients
have not changed yet) and `staged_total{outcome="refused"}` stops moving.

## 5. M2 — Apple

PR title: `feat(apple): wait for the prepared offer with the picture up`.
Bump `CURRENT_PROJECT_VERSION` (165 → next). No `fix(` in the subject.

### 5.1 The wait, as a pure type

Add to `clients/apple/Sources/PreparedReplacement.swift` (the file that is
"free of AVFoundation and unit-tested"):

```swift
/// What a directed selection change is waiting for after it has been
/// published, read off each exchange's answer. Pure: the controller feeds it
/// answers and the clock, it says what to do.
struct PreparedOfferWait: Equatable {
    enum Step: Equatable {
        /// Keep the incumbent playing; exchange again after `nextExchangeMs`.
        case keepWaiting(nextExchangeMs: Int)
        /// A `Prepare` for this ask arrived — hand it to the coordinator.
        case offered(PreparedReplacementAction)
        /// The server said `none` after accepting the ask, or the bound
        /// expired: reopen in place now, at the CURRENT film position.
        case reopen(reason: String)
    }
    /// D1: twelve seconds from the tap.
    static let boundMs = 12_000
    /// 1 Hz while the server says `staging`.
    static let stagingCadenceMs = 1_000

    let tappedAtMs: Int
    let floorSequence: Int
    private(set) var sawAcceptedAnswer = false

    mutating func observe(
        answer: PlaybackControlAnswer?,   // requestSequence, action, preparation
        nowMs: Int
    ) -> Step
}
```

Semantics, each of which is a test case:

1. `nowMs - tappedAtMs >= boundMs` → `reopen("timed_out")`, checked before
   anything else — including before any exchange has been accepted.
2. An answer with `requestSequence < floorSequence` is not ours → `keepWaiting`.
3. Our answer with `action` = `Prepare` → `offered`, regardless of
   `preparation`.
4. Our answer with `preparation == "none"` → `reopen("declined")`, but only
   on an exchange *after* the one that carried the ask: the dispatch exchange
   itself answers `staging` on a server with §4.1 and may answer absent on an
   older one, so the first accepted answer sets `sawAcceptedAnswer` and
   returns `keepWaiting`; a later `none` declines.
5. Our answer with `preparation == "staging"` (or absent, an old server) →
   `keepWaiting(nextExchangeMs: stagingCadenceMs)`; the bound decides for
   an old server.

### 5.2 Wiring, and where the fallback lands (this settles R2)

- `ControlDelivery` (`PlaybackControlReporter.swift:601`) gains `var
  preparation: String?`. `PlaybackControlAnswers.Answer`
  (`PlaybackControlSession.swift:551-556`) gains `preparation: String?`, and
  `record(...)` (`:573`) is called with `exchange.response?.delivery?.preparation`
  from the `onExchange` hook (`:274`).
- Add `func awaitPreparedOffer(tappedAt:publish:) async ->
  PreparedOfferWait.Step` to `PlaybackControlSession` beside `askForAction`
  (`:389`). Same floor discipline (`floor = reporter.sequence + 1` read
  **before** `publish()`), same `ownerChangeCount` bail-out, same
  `stopped` re-read; the loop feeds `answers.latest` to a `PreparedOfferWait`
  every `askPollNanoseconds` and, whenever the step is `keepWaiting` and the
  last answer said `staging`, calls `reporter.notify()` no more often than
  once per `stagingCadenceMs`. **Do not widen `askForAction`'s bound** — its
  1.5 s is a stall policy and the two must stay separate.
- `selectQuality` / `selectOriginalQuality` (`PlayerController.swift:3214`,
  `:3251`) today compute `destination = seekState.absolute(positionForPlaybackIntent(), …)`
  **before** the await and reopen at `destination.target` after it
  (`:3222-3226`, `:3247`). That was correct for an immediate reopen; after a
  12 s wait it rewinds the film by the wait. Change: a quality-only change
  does not pin a destination. Record `let seekGeneration = seekState.generation`
  before the await; after a `.reopen` step, if `seekState.generation ==
  seekGeneration` (no viewer seek meanwhile) reopen at
  `positionForPlaybackIntent()` **sampled then**; if a seek happened, the seek
  owns the position and this change does nothing (the seek's own path already
  carries the new `selectedHeight` because `recipeRevision.change()` ran at
  the tap). Keep the cold-start branch (`:3230-3235`) as it is.
- `offerPreparedQualityChange` (`:3298`): replace the `askForAction` call
  with `awaitPreparedOffer`. On `.offered(action)` keep the existing
  `preparedReplacement.offer(action, filmPositionMs:)` and return
  `ownsTheChange`; on `.reopen` return `false`. The reporter's own push
  (`onPreparedReplacement`, `:8238-8250`) still runs and stays idempotent with
  this path exactly as today.
- Cancel the wait on a viewer action: make `awaitPreparedOffer` return early
  when `viewerActionEpoch` moves, so a seek during the wait reopens at the
  seek target immediately instead of after the bound. If a `Prepare` for the
  abandoned ask arrives later, `PreparedReplacementCoordinator` already
  settles it (`abandon(.aborted)` on supersede) — verify with a test that the
  `aborted` acknowledgement is queued, not dropped.
- `Caps.swift:288-294` / `LiveTvDeveloperView.swift:30-42`: add the two
  advisory rows from the plan §5.5 — last outcome (`committed <ms>` ·
  `declined` · `timed out` · `fell back`) and the last `preparation` value.
  Advisory, never gating.

### 5.3 Ownership after the offer (this settles R3 for Apple)

Apple already has it: every failure after an offer reaches
`host?.fallBackToInPlaceReplacement(action)` (`PreparedReplacement.swift:513`,
`:550`, `:557`, host at `PlayerController.swift:8743`), which reopens at
`positionForPlaybackIntent()` under the viewer-action epoch — so a directed
change that was offered and then failed ends in exactly one reopen, and a
superseded one in none. Two things to verify rather than build: that the
epoch check in `fallBackToInPlaceReplacement` refuses when a newer viewer
action owns the player (test: supersede between failure and fallback), and
that `switchedWithoutAFrame` still reopens rather than only recording its
interruption (`:551-557`).

### 5.4 The switch itself is unchanged (D4), and the rendezvous is checked

`commitPreparedSuccessor` (`:8603-8683`) and `replaceCurrentItem` at `:8659`
stay. Do not add a second `AVPlayerLayer`. One check, because R4/R5 apply to
every client: the successor item is seeked once to `preparedSeekMs` and never
played (`:8454-8462`, `:8471-8474`), so at commit its position is the staged
film position while the incumbent has moved on. Read `:8626-8660` and
confirm how the item is placed on the incumbent's current position before
exposure — whether the commit re-seeks the item to `realPositionMs()` and
awaits it, or relies on `boundaryMs = max(preparedFilmPositionMs,
realPositionMs())` (`:8647`) with a tolerance. If exposure can precede a
completed seek, apply §7.3's rule here too; if it cannot, record the
mechanism in the PR so M3 knows what it is measuring. The 20/20 hardware
result says it works; the plan should say why.

### 5.5 Proof for M2 Apple

- `clients/apple/Tests/`: `PreparedOfferWaitTests` covering the five
  semantics above plus: bound expiry before any accepted exchange; old
  server (no field) waits out the bound; an owner change ends the wait; a
  `Prepare` on a *replayed* older sequence is not taken. Run the new tests
  against a `PreparedOfferWait` whose `observe` ignores `preparation` and
  confirm cases 4 and 5 fail.
- Position: a controller-level test (or a pure helper extracted for it) that
  advances the incumbent's position during a simulated wait, forces `.reopen`,
  and asserts the reopen position is the advanced one; and with a seek during
  the wait, asserts the seek's target wins and the quality path reopens
  nothing.
- Ownership: offer → `startPreparedSuccessor` returns false → exactly one
  reopen; offer → readiness bound → exactly one reopen; offer → supersede →
  no reopen from this path.
- `make apple-test` on the macOS runner green (~900 cases, `EXIT=0`).
- The reducer transliteration technique from the playback-surface PRs applies
  to `PreparedOfferWait` if the reviewer cannot compile Swift: port it to
  Python and run the same cases.
- Hardware: run [M6-APPLE-HARDWARE-ACCEPTANCE.md](M6-APPLE-HARDWARE-ACCEPTANCE.md)
  on the fleet build carrying M0, on an iPhone and an Apple TV; the acceptance
  is `plurx_playback_control_actions_total{action="prepare",platform="apple"}`
  incrementing once per menu change and one `committed` per change in the
  server log, with the client log's `first_frame_unix_ms - tap` under a
  second. This is a GPT-session task with device access; write the prompt
  into the PR.

## 6. M2 — Android

PR title: `feat(android): wait for the prepared offer, and meet the
successor at a rendezvous`. Bump `versionCode` (103 → next).

### 6.1 The wait

Add `PreparedOfferWait` to
`clients/android/app/src/main/java/tv/plurx/app/player/PreparedReplacement.kt`
with the same five semantics as §5.1 (Kotlin: a class with
`fun observe(answer: ControlAnswer?, nowMs: Long): Step`, `Step` a sealed
interface `KeepWaiting(nextExchangeMs)` · `Offered(action)` ·
`Reopen(reason)`; `BOUND_MS = 12_000L` from the tap,
`STAGING_CADENCE_MS = 1_000L`). It lives here because this file is the one
the JVM lane can construct; `Controller.kt` is not (see the M6 Android
status: every defect that survived review lived there).

- `ControlDelivery` (`PlaybackControlReporter.kt:667`) gains
  `@SerialName("preparation") val preparation: String? = null`.
- `PlaybackControlSession` exposes the per-exchange answer (sequence, action,
  `delivery.preparation`) the way it already dispatches `onPrepare`
  (`PlaybackControlSession.kt:404-412`); add `suspend fun
  awaitPreparedOffer(tappedAtMs: Long): PreparedOfferWait.Step` beside the
  stall ask (`Controller.kt:1666-1676` uses it), feeding the pure type,
  calling `notifyUrgently()` at most once per `STAGING_CADENCE_MS` while the
  last answer said `staging`. Re-throw `CancellationException` explicitly
  inside any `try/catch (Exception)` on this path — the M6 review found
  exactly that trap in this file.

### 6.2 `prepareReplacement` waits, and the fallback resumes where the viewer is (R2, R3)

`Controller.prepareReplacement` (`Controller.kt:1187-1207`) today records the
change as a pending *seek* — `playbackIntent.beginSeek(positionForPlaybackIntent(positionMs), realPosition(), quality)`
— and then routes the plan replacement unconditionally. Two changes:

1. **A quality-only change is not a seek.** Add
   `PlaybackIntent.beginQualityChange(quality)` beside `beginSeek`
   (`PlaybackIntent.kt`, cf. `adoptQuality` at `:46-51`): it records the
   desired quality and the tap time and leaves the position intent alone, so
   a fallback samples `realPosition()` **when the reopen starts**. A viewer
   seek during the wait goes through `beginSeek` as today and owns the
   position; the quality change rides along with it because the quality is
   already part of the intent.
2. **Wait, then own the outcome.** Replace `if (current)
   planReplacement.route(playbackIntent, force = true)` with:

```kotlin
scope.launch {
    val current = publishIntent(pending, quality, publicationEpoch)
    if (!current) return@launch
    directedChange = DirectedChange(epoch = publicationEpoch, quality = quality)   // §6.3
    when (val step = playbackControl.awaitPreparedOffer(tappedAtMs)) {
        is PreparedOfferWait.Step.Offered -> onPrepareAction(step.action)   // :2996 builds it; idempotent with the reporter's push
        is PreparedOfferWait.Step.Reopen -> directedChange?.fallBackOnce(reason = step.reason)
        is PreparedOfferWait.Step.KeepWaiting -> error("awaitPreparedOffer returned a non-terminal step")
    }
}
```

`fallBackOnce` (below) routes `planReplacement.route(playbackIntent, force =
true)` with the position sampled at that moment, exactly once, only while
`mediaMutationEpoch == epoch`.

### 6.3 `DirectedChange` — one owner from tap to commit or one reopen (R3)

Today a preparation that fails after the offer only releases the successor and
acknowledges `failed` (`Controller.kt:3010`, `:3397`, `releaseSuccessor` at
`:3406`); the viewer's chosen quality is never applied. Add to
`PreparedReplacement.kt` (pure, JVM-testable):

```kotlin
/** The viewer's directed change, alive from the tap until it is honoured. */
internal class DirectedChange(val epoch: Long, val quality: PlaybackQuality) {
    private var settled = false
    /** Commit honoured it: nothing more to do. */
    fun committed() { settled = true }
    /** Any terminal failure of the offered successor, or the wait ending
     *  without an offer: exactly one ordinary reopen, if this change is
     *  still the viewer's current intent. Returns whether it routed. */
    fun fallBackOnce(reason: String, currentEpoch: Long, route: () -> Unit): Boolean
    /** A newer viewer action owns the player; this change does nothing more. */
    fun superseded() { settled = true }
}
```

Wire it: `Controller.directedChange` is set in §6.2; `commitPreparedReplacement`'s
first-frame settlement (`settleCommitOnFirstFrame`, `:3295-3303`) calls
`committed()`; every path that reaches `preparedLedger.failed()` /
`failedAfterSwitch()` / readiness-bound expiry / `rollbackSwitchedReplacement`
(`:3010-3026`, `:3313-3353`) calls `fallBackOnce`; every entry in the
stream-replacing list (`:3362-3377`) calls `superseded()`. A preparation that
did **not** come from a directed change (a server-initiated one, if any ever
exists) has no `DirectedChange` and keeps today's release-only behaviour.
Log `quality_switch` with `via=prepared|fallback|declined|timed_out` through
the existing client-log event so the fleet can be read.

### 6.4 Rendezvous instead of a chase (this settles R4)

Today `pollPreparedReplacement` freezes the incumbent (`player.playWhenReady
= false`, `Controller.kt:3133`, `:3142`) while the successor seeks onto its
position — a visible pause on every prepared switch. The first revision of
this plan proposed seeking the successor *ahead* and letting them run; that
does not converge: two pipelines at the same rate keep whatever lead the
seek landed with, so a 1.5 s lead never closes to the 250 ms slack.

The correct shape is a **rendezvous hold**:

1. Pick the rendezvous point `R = incumbentFilmMs + RENDEZVOUS_LEAD_MS`
   (`RENDEZVOUS_LEAD_MS = 1_500`, chosen to cover a seek on a Google TV with
   margin; make it a constant with the reason).
2. Seek the successor to `successorAttachPositionMs(originMs, R)` **with
   `playWhenReady = false`** — the successor is parked at `R`, not chasing.
   Wait for `onPositionDiscontinuity(DISCONTINUITY_REASON_SEEK)` (`:3052-3063`)
   and for `successorIsBuffered(bufferedThrough, R)` (`PreparedReplacement.kt:310-314`)
   — buffer lead and presentation alignment are separate requirements and
   both are checked at `R`, not at the moving playhead.
3. The incumbent keeps playing. Schedule the swap for when it reaches `R`:
   `delayMs = (R - incumbentFilmMs) / playbackParameters.speed`, via a
   coroutine delay (the 1 s tick at `:2408` is too coarse for a 250 ms
   window). At fire time re-read `incumbentFilmMs`; if `|incumbentFilmMs - R|
   <= PREPARED_ALIGNMENT_SLACK_MS`, commit: `successor.playWhenReady = true`
   and the existing `commitPreparedReplacement(incumbentFilmMs)` (`:3160`).
   If the incumbent has not reached `R` (paused, rate changed), reschedule
   once from the new reading; if it has passed `R` (a seek forward, a rate
   above 1), pick a new `R` and re-park — **at most twice**, then
   `preparedLedger.failed()` → `fallBackOnce` (§6.3).
4. A viewer pause during the hold simply postpones the fire (the delay is
   recomputed from the current rate each time); a viewer seek supersedes
   (§6.3).

Remove the two freezes and their restores in `releaseSuccessor` (`:3408`,
`:3417`). Web is not the model here — see §7.3, it has its own defect.

### 6.5 Proof for M2 Android

- JVM tests in `app/src/test/…/PreparedReplacementTest.kt` (or the existing
  file for `PreparedReplacementLedger`): the five `PreparedOfferWait`
  semantics, bound expiry before any accepted exchange, old-server behaviour,
  superseded epoch; `DirectedChange` — commit → no fallback, failure → one
  fallback, failure twice → still one, supersede → none; and a **rendezvous
  model**: extract the scheduling arithmetic (`R`, `delayMs`, the fire-time
  acceptance, the re-park rule) as a pure class driven by two simulated
  clocks, and prove convergence for a slow seek (2 s), a fast seek (200 ms),
  rates 0.5×/1×/2×, a pause during the hold, and the bounded failure after
  two re-parks. Run the model tests against the abandoned "seek 1.5 s ahead
  and let it run" arithmetic and confirm they fail — a test of target
  arithmetic alone is not proof of convergence.
- `./gradlew --no-daemon :app:testDebugUnitTest :app:assembleDebug
  :app:lintDebug` green; read counts from
  `app/build/test-results/testDebugUnitTest/*.xml`.
- Settings → Developer → "Prepared replacement": the two advisory rows
  (§5.5 of the plan). `preparedReplacementEnabled` stays read once per player
  (`Controller.kt:562-576`); say so in the row's text rather than changing it.
- Hardware: an Android twin of the Apple acceptance on the Google TV and a
  phone, same metric acceptance as §5.5. GPT-session task; write the prompt
  into the PR.

## 7. M1 — web (after M0 merges into the effort)

PR title: `feat(web): the quality menu and Auto go through the prepared
handoff`. Two commits: the client, then the D3-a server half.

### 7.1 `requestPreparedQualityChange` — a multi-exchange waiter

`askPlaybackControl` (`index.html:8791`) cannot be reused as is: its waiter
settles on the **first** exchange at or after its `minSequence`
(`settlePlaybackControlWaiters`, `:8855-8877`), and the `Prepare` arrives on a
*later* exchange than the one that carried the ask. Add a second waiter kind
beside it — same `controlWaiters` list, same floor discipline, but it stays
armed across exchanges and is settled by rule:

```js
// A directed selection change. Publishes the new selection and keeps the
// incumbent playing while the server builds a successor; resolves
// "prepared" | "declined" | "timed_out" | "superseded". The caller owns the
// fallback (see requestQualityChange below).
const PREPARED_OFFER_BOUND_MS=12000;      // D1, from the tap
const PREPARED_OFFER_CADENCE_MS=1000;
async function awaitPreparedOffer(p,tappedAt){ … }
```

Rules, each a `web-control.test.js` case:

1. `minSequence` is read before `notifyPlaybackControl()`, as
   `askPlaybackControl` does (`:8809-8813`).
2. `p.mediaAttachment` is **not** touched. That guard (`:8937`, `:8945-8947`)
   is what dropped the `Prepare` in the plan's §2.2; the reporter must keep
   owning the media for the whole wait.
3. In `settlePlaybackControlWaiters`, an offer waiter is settled — not
   extended — by: `action.type === PREPARE_ACTION_TAG` → `"prepared"` (the
   existing `onExchange` branch at `:8960-8967` builds it; do not build it
   twice — check `preparedState(p)` for the same `action_id`);
   `delivery.preparation === "none"` on an exchange *after* the accepted ask
   → `"declined"`; `"staging"` → stay armed and schedule `reporter.notify()`
   after `PREPARED_OFFER_CADENCE_MS`; absent field → stay armed (old server;
   the bound decides). An exchange failure does **not** settle it (unlike the
   stall waiter) — the incumbent is fine, only the ask is unanswered; the
   `owner_changed` clear at `:8862-8865` does settle it, `"superseded"`.
4. `p.controlIntentGeneration` changing (a seek, another menu choice) →
   `"superseded"`.
5. `performance.now() - tappedAt >= PREPARED_OFFER_BOUND_MS` → `"timed_out"`;
   if a `Prepare` was seen and not yet built, queue `aborted` for it first
   (`queuePlaybackControlAcknowledgement`, `:7942`).

### 7.2 `requestQualityChange` — the owner (R2, R3)

```js
// One owner per directed change, from the tap to a commit or ONE fallback.
// `p.directedChange` — {intentGeneration, tappedAt, settled}.
async function requestQualityChange(p,reason){
  const change={intentGeneration:p.controlIntentGeneration,tappedAt:performance.now(),settled:false};
  p.directedChange=change;
  const outcome=await awaitPreparedOffer(p,change.tappedAt);
  if(outcome==="prepared") return;            // commit or failure settles it (7.3)
  if(outcome==="superseded") { change.settled=true; return; }
  fallBackDirectedChange(p,change,outcome);
}
function fallBackDirectedChange(p,change,why){
  if(change.settled||p.directedChange!==change||p.controlIntentGeneration!==change.intentGeneration) return;
  change.settled=true;
  const v=document.getElementById("video");
  const pos=positionForPlaybackIntent(v,p);   // sampled NOW, not at the tap (R2)
  PENDING_ATTEMPT_REASON="quality";
  clientLog({event:"quality_switch",detail:`via=fallback why=${why}`,…});
  play(p.fileId,p.title||"",Math.round(pos*1000),p.knownDur||0,p.meta);
}
```

Call sites:

- `setQuality` (`:13752`): keep the toast and the log; drop the `pos` capture
  and the `play(...)` at `:13766-13768`; call `beginPlaybackControlSeek`
  **only** if a seek is actually pending (it is not, for a quality-only
  change — today's call at `:13767` marks a seek that is not one), then
  `requestQualityChange(PLAYER,"manual")`.
- `switchAutoRung` (`:11864`) and `rescueAutoSupply` (`:11880`): the same
  owner, with the fallback being today's `requestPlaybackMediaChange(...)`
  rather than `play()`, **only when the incumbent is not stalled** — the
  server cancels preparations on `Waiting|Stalled` anyway
  (`hls.rs:7096-7107`), so a stalled reopen stays a reopen and does not
  spend 12 s. Needs §7.4 to be expressible.
- Ownership after the offer: `commitPreparedReplacement`'s first-frame
  settlement (`:8291-8292`) marks `p.directedChange.settled`;
  `failPreparedReplacement` (`:8455-8466`), which today only acknowledges,
  logs and frees, calls `fallBackDirectedChange(p,p.directedChange,"failed")`
  when one is current; `abandonPreparedReplacement` from a stream-replacing
  path (`:8472`) marks it superseded. A preparation with no `directedChange`
  keeps today's behaviour.

### 7.3 Finish alignment before exposing the successor (this settles R5)

`commitPreparedReplacement` (`:8203-8290`) measures drift, sets
`spare.currentTime` when it exceeds `PREPARED_ALIGN_SLACK_MS` (`:8210-8212`),
and then **in the same synchronous block** exposes and unmutes the successor
and hides and mutes the incumbent (`:8246-8266`). It does not await the
`seeked` event or re-check buffered media after the seek, so a corrective
seek exposes a successor that has nothing decoded at its new position. The
runway measured before alignment (`:8185-8191`) is not evidence about the
position after it.

Change the commit into two phases:

1. **Align, incumbent still visible and audible.** If drift exceeds the slack,
   seek the successor and `await` its `seeked` event with a bound
   (`PREPARED_ALIGN_SEEK_MS = 1500`); then re-check `spare.readyState >=
   HAVE_FUTURE_DATA` and that `spare.buffered` covers `[spare.currentTime,
   spare.currentTime + PREPARED_BUFFER_LEAD_MS / 2]`. Re-measure drift once
   more against the incumbent (it kept moving during the seek); if it is
   still over the slack, seek again — at most twice — then
   `failPreparedReplacement(p,state,"could not align")`, which now falls back
   (§7.2). The web runs its successor muted and *playing* (`:8127`), so the
   drift after a completed seek is only the seek's own latency; the
   rendezvous hold of §6.4 is not needed here, but the same two-attempt bound
   is.
2. **Expose**, exactly the existing block (`:8217-8290`): intent sampled at
   the last reversible boundary, mute/volume/rate applied before display,
   element swap, `adoptPlaybackMediaElement`, `spare.play()`, first-frame
   settlement and rollback unchanged.

### 7.4 D3-a — Auto carries the rung it wants (server half, same PR; R6)

**Server.** `QualitySelection` (`playback_control.rs`, the `quality` field of
`ClientSelection` at `:380`) gains an optional `height` on its `Auto` arm:

```rust
Auto { #[serde(default, skip_serializing_if = "Option::is_none")] height: Option<i64> }
```

`desired()` (`:390`) maps it to `DesiredQuality::Auto { height }` — the enum
lives in `crates/plurx-core/src/playback/desired.rs:45` and its `digest()`
(`:178`) is what `take_preparation_dispatch` compares (`playback_control.rs:3767`),
so the height must reach the digest for an Auto controller moving 720 → 1080
to be a `SelectionChange`. Keep `Auto { height: None }` digesting to exactly
today's string, or every current session records a spurious change on the
first exchange after deploy. `candidate_request` (`playback_control.rs:1233`,
the M6 helper that edits a `SessionRequest` for the successor) uses the height
as an explicit ask while leaving `quality_auto = true` on the recipe, so the
server's own Auto policy (`resolve_height`'s Auto arm, `hls.rs:1512`) is not
switched off for the successor. `resolve_height` itself is unchanged: an
explicit `height` with `quality_auto` still snaps to the ladder (`:1518`) and
never binds above the capability ceiling. `validate` accepts `height` absent
(every current client) and rejects it outside `144..=2160` exactly as
`manual` does.

**Web — the requested rung is new state, and it is set before the notify.**
There is no `PLAYER.autoRungHeight` today; `switchAutoRung(p, decision)`
receives the chosen rung as `decision.height` (`:11864-11879`) and
`PLAYER.autoHeight` is the *delivered* height the controller refreshes. Add
`p.autoRequestedHeight`:

- **Set** in `switchAutoRung` (and `rescueAutoSupply` when it chooses a rung)
  to `decision.height` **before** `requestQualityChange` runs, so the first
  exchange after the decision already carries it — `playbackControlSelection`
  (`:7706`) emits `{mode:"auto", height:p.autoRequestedHeight}` when it is
  set and the saved quality is Auto, `{mode:"auto"}` otherwise.
- **Cleared** on commit (the delivered height now is the requested one, and
  the selection returns to plain Auto — a stable digest, not a new ask), on
  the directed change's fallback (the reopen's create carries the height
  explicitly, as today), on supersession, and when the viewer picks anything
  in the quality menu.
- Never copied into `p.autoHeight`; requested and delivered stay distinct,
  and the Developer row shows both.

The conformance test's `test_the_selection` (`:320`) pins the field names —
add `height` to the `auto` arm in every port's model **only if that port
declares it**; Apple and Android keep sending `{mode:"auto"}` and are
unaffected.

### 7.5 Proof for M1

- `node --test tests/playback/web-control.test.js`: the five waiter rules in
  §7.1; the owner in §7.2 — menu → offer → commit → no fallback; menu → offer
  → `failPreparedReplacement` → one fallback at the *advanced* position
  (drive `video.currentTime` forward during the simulated wait and assert the
  `play()` position); menu → `none` → one fallback; menu → bound → one
  fallback; menu → seek during the wait → no fallback from this path; Auto
  while stalled → plain reopen; the §7.3 alignment — a corrective seek whose
  `seeked` is delayed leaves the incumbent displayed and audible until it
  completes, and a seek that never completes falls back with the incumbent
  intact. Run the §7.3 cases against the current single-phase commit and
  confirm they fail. `shippedSource(...)` pins for every new function name.
- `make web-check`; `tests/ui-structure.golden` is unaffected (no new DOM at
  rest — the Developer rows live inside an existing card).
- Rust: `cargo test -p plurxd quality_selection` for the `Auto { height }`
  serde round-trip, the unchanged digest for `height: None`, the digest
  change for a height, and the `candidate_request` case; the conformance
  test.
- End to end for R6: drive a real Auto decision 720 → 1080 through
  `switchAutoRung` in the test harness and assert, in order, that
  `p.autoRequestedHeight` is set before `notifyPlaybackControl` runs, that
  the outgoing selection carries `height: 1080`, and (Rust side) that the
  digest changes and the staged recipe carries 1080.
- Browser acceptance against nynuc: `plurx_playback_control_actions_total{action="prepare",platform="web"}`
  increments once per menu change, one `committed` per change; the client log
  `quality_switch` carries `via=prepared`.

## 8. M3 — measure the switch

Instrument all three clients and report through the Developer rows and the
client log (`playback-info-fields.json` gains a `PREPARED SWITCH` group so
all three ledgers show it):

| Measure | Web | Apple | Android |
|---|---|---|---|
| Frames presented, ±2 s around commit | `getVideoPlaybackQuality().droppedVideoFrames` delta on the successor element | `AVPlayerItemVideoOutput` frame count on the item across `replaceCurrentItem` | `Player.Listener.onDroppedVideoFrames` on the successor |
| Audio discontinuity | `AnalyserNode` on a `MediaElementAudioSourceNode` — gap ≥ 20 ms of silence inside 300 ms of the swap | `AVPlayerItemAccessLog` stall count + an `MTAudioProcessingTap` gap detector if the log is silent | `AudioSink` underrun via `AnalyticsListener.onAudioUnderrun` |
| Tap → new quality on screen | `directedChange.tappedAt` to `first_frame` | `viewerActionEpoch` timestamp to `first_frame_unix_ms` | `DirectedChange` tap time to `onRenderedFirstFrame` |

Acceptance is §1's bar, twenty consecutive directed changes per platform on
the fleet build, recorded in a results doc in this folder
(`QUALITY-SWITCH-CONTINUITY-RESULTS.md`, indexed). If Apple's item swap
drops frames, that result — not this document — opens the second-layer
cross-fade as a follow-up.

## 9. Guardrails — do not

- Change the `Prepare` wire shape, `ActionAcknowledgement`, or add an action
  type. Apple's reporter fatals on undeclared action types
  (`PlaybackControlReporter.swift:1077-1110`).
- Change `NEXT_EXCHANGE_MS` or `renew_after_ms`; every client pins it in its
  validator. The 1 Hz cadence is client-initiated `notify`.
- Reuse `hold` for "preparing". `hold` means production is deliberately not
  advancing and clients treat it as a reason not to reopen a stalled session.
- Await the staging inside the exchange (`http/hls.rs:7210`). The 4 s exchange
  deadline is already spent by then.
- Widen the 1.5 s stall ask (`askForAction`, `CONTROL_ASK_MS`,
  `Controller.kt` `CONTROL_ASK_MS`) to carry this wait. Separate function,
  separate bound.
- Pin the fallback position at the tap. Sample it when the reopen starts.
- Gate any of this in code. Developer rows are advisory.
- Touch `PREPARATION_DEADLINE_MS`, `PREPARATION_PRIME_BUDGET`, or the
  admission priorities; the successor stays `Speculative` and pre-emptible.
- Add a second `AVPlayerLayer` (D4) or a multivariant master playlist (plan
  §7).

## 10. Hand-back

Each PR's description carries: the focused regression command that was run
and its counts; the review findings and how each was closed; for M2, the
paste-ready hardware prompt for a GPT session with device access. When M3's
results doc exists, update the M6 row in
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) and the status of
this document and the plan to `built`.

## 11. Review disposition — Astra, 2026-09-16

| Finding | Accepted? | Revised section | Regression that closes it |
|---|---|---|---|
| R1 pending planning must count as `staging` | Yes | §4.1 pending-candidate marker (RAII guard from spawn to registry hand-over) | §4.3 `delivery_preparation_stays_staging_while_the_candidate_is_planning`, including supersession during the delay |
| R2 fallback must resume where the viewer has reached | Yes | §5.2 (Apple: no pinned destination for a quality-only change), §6.2 (`beginQualityChange`, not `beginSeek`), §7.2 (`fallBackDirectedChange` samples position at reopen); guardrail in §9 | §5.5 position test; §6.5 `DirectedChange` + position; §7.5 advanced-position fallback and seek-during-wait cases |
| R3 a directed change needs an owner after `Prepare` | Yes for web and Android; Apple already has it (`fallBackToInPlaceReplacement`, verified `PreparedReplacement.swift:513/550/557`) | §6.3 `DirectedChange`, §7.2 `requestQualityChange` owner, §5.3 verification items | §6.5 and §7.5 "exactly one fallback / none when superseded" cases across construction, readiness and first-frame failures |
| R4 Android's fixed lead does not converge | Yes — the first revision's "seek ahead and let it run" was wrong | §6.4 rendezvous hold: park the successor at `R`, schedule the swap for the incumbent's arrival, bounded re-park, then fail → one fallback | §6.5 two-clock model: slow/fast seek, 0.5×/1×/2×, pause, bounded failure; proven against the abandoned arithmetic |
| R5 web must finish alignment before exposing | Yes — the web block is no longer "unchanged" | §7.3 two-phase commit: align with `seeked` awaited and buffer re-checked, bounded, then expose | §7.5 delayed-`seeked` and never-completes cases, proven against the single-phase commit |
| R6 Auto needs an explicit requested rung | Yes — `PLAYER.autoRungHeight` did not exist | §7.4 `p.autoRequestedHeight`: set before notify, lifetime on commit/fallback/supersede/menu, distinct from `autoHeight` | §7.5 end-to-end 720 → 1080 with set-before-notify assertion and Rust digest/recipe checks |
| R7 twelve seconds needs one meaning | Yes | §1 two bounds: offer bound 12 s from the tap; M6's readiness/first-frame bounds after, each now ending in the one reopen; plan §3 and §5.1 corrected to match | §5.5/§6.5/§7.5 bound-before-any-accepted-exchange cases; ownership cases cover "offer then never ready" |
| R8 M0 tests must exercise real cancellation ownership | Yes | §4.3 `register_test_preparation` constructor; the guard test runs with a matching entry present; the post-commit test is demoted to a lifecycle check | §4.3 `the_activating_successor_is_kept_by_the_supersession_cancel` with its named mutation; pending-marker cases |
| Workflow — bounded exception misapplied | Yes | §3: one `effort/quality-switch-continuity` branch per AGENTS.md; ownership table binds within it | `Effort development gate` per task PR; `Main promotion gate` on the effort |

Settled decisions D2 (field) and D4 (`replaceCurrentItem`) are unchanged by
the revision; no correction required expanding scope into the wire or the
Apple switch primitive. The one scope addition the review forced is §7.3 —
the web commit's alignment — which the first revision wrongly listed as
already correct.
