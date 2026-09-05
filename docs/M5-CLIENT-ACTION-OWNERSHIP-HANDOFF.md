# M5 — moving recovery authority off the clients

**Status:** ready to build · **Executes:** item 6 of
[PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md)
§6, which is step 5 of
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) §9.3 ·
**Written:** 2026-08-31 · **Prerequisite:** migration step 4, complete on
`main` at `bc504681`

Companion to the protocol plan (what the system must become) — this is *the
milestone where the clients stop deciding for themselves*.

## 1. Orientation — read this, work like this

Read, in order: plan §7.4 (the deletion ledger, one table per platform); §2 of
this file, which records what the wire can now say and is the thing that makes
this milestone possible at all; then §3, which is the live recon of the web
client so the first day is not spent rediscovering it.

**One platform per PR.** Web, Apple, Android are independent. Do not attempt
two in one branch: the mobile build-number gate (§6) forces a bump on any
client change and rejects a bump computed against the commit you branched
from, so two client PRs in flight means the second always fails.

**Do not delete anything until its replacement is proved.** The plan's §7.4
disposition column says what each owner becomes, not merely that it goes. An
owner deleted without its replacement is an outage on the next real stall.

**The fleet must be running the new clients first.** See §5 — there is a metric
for this, and it currently reads zero.

## 2. What the wire can now say — as of 2026-08-31

This is the whole reason M5 can start. Twelve hours ago `ControlAction` had one
variant and it was `None`; there was nothing for a client to defer to.

| action | meaning | derived from |
|---|---|---|
| `none` | nothing to say | — |
| `hold { reason }` | production is deliberately not advancing | `DeliveryView.hold_reason` — seven values |
| `retry_resource { after_ms, reason }` | production stopped for something that may not recur | a non-permanent `ProducerDecisionReason` |
| `terminal { code, message }` | production stopped for a reason retrying cannot change | `unsupported` or `invalid_configuration` |

`ControlAction` and `resolve_action` are in
`crates/plurxd/src/playback_control.rs`. Three properties are already tested
and must not be broken:

1. **A stopped producer outranks a hold.** A client told to hold while the
   producer is finished would sit through a film that was never going to play.
2. **An undeclared verdict is withheld, not softened.** A client that declares
   only `hold` is told nothing rather than told to hold.
3. **A relayed verdict must match both the decision the same response reported
   and its permanence.**

### 2.1 The distinction M5 depends on

`producer_state` says `failed`. That single word covered every distinct
producer decision until 2026-08-31, which is exactly why no client could decide
anything. There are **nineteen** of them now that VOD failures are classified
too: sixteen are timing, process, plan or executor facts where the next attempt
may work, and three cannot be retried into working — two verdicts about the
source, and `EngineChanged`, which is a verdict about this daemon process.
`DeliveryView.producer_decision` now carries which one it was, and
`ProducerDecisionReason::is_permanent()` is the split.

**This is the fact every §7.4 owner was guessing at.** Each of them, on every
platform, guesses toward retry — reopen, same verdict, reopen again.

### 2.2 What clients do with the actions today

All three declare `["hold", "retry_resource", "terminal"]` and:

- `hold` — accepted, no behaviour. It exists so a deliberate pause is not read
  as a fault.
- `retry_resource` — paces the next exchange to `after_ms`.
- `terminal` — stops the **reporter**. It deliberately does **not** touch the
  player: buffer already fetched is still worth playing, and the reporter owns
  no recovery.

**Making `terminal` end playback is M5's job, not something already half-done.**

## 3. Web — live recon, 2026-08-31

`crates/plurxd/src/web/index.html`. Verify line numbers at build time; the file
moves constantly.

### 3.1 What is actually automatic, and what is not

This distinction matters more than the symbol list, and §7.4 does not draw it.

| symbol | line | automatic? |
|---|---|---|
| `persistentWait` | ~3424 | **Yes.** Calls `PlaybackPolicy.stallRecoveryAction(...)`; either prompts or silently reopens at a lower rung |
| `handleEnded` truncated branch | ~8947 | **Yes.** Resumes up to `endedTries > 3`, then prompts |
| `stallDiagnose` | ~8350 | **No.** Diagnoses and shows *Try again / Force transcode / Close*. This is viewer intent — §7.4 classifies it `no` and says retain it |
| `pollSessionHealth` | ~10126, 2 s interval | Reads `/status`. Not a recovery decision by itself; M9 deletes it |

**Do not delete `stallDiagnose`.** Its verdicts ("the stream request looks
blocked", "a codec this browser can't decode") are the only thing that tells a
viewer an ad-blocker is eating `/hls/*.ts`. A server action can never know
that. What it must lose is nothing — it already only prompts.

### 3.2 The two automatic owners already report

Both already call `notifyPlaybackControl(...)` (~6276) before acting:

```js
// persistentWait
const controlObservation = kind === "supply"
  ? { decoder_state: "starved" }
  : { decoder_state: "failed", error_code: "decoder",
      error_detail: "persistent_decode_stall" };
const controlTrigger = notifyPlaybackControl("stalled", controlObservation);
```

The comment above it is worth reading before changing anything: that exact
transition owns the legacy reopen, and cadence alone is too late because the
replacement normally stops the reporter before its next scheduled exchange.

**So the observation half of §7.4 is done.** What remains is the decision half:
the line after the observation must ask the controller instead of
`PlaybackPolicy`.

### 3.3 The shape of the web change

```
 before                          after
 ──────                          ─────
 persistentWait                  persistentWait
   ├─ notifyPlaybackControl        ├─ notifyPlaybackControl
   └─ PlaybackPolicy               └─ await the controller's action
        .stallRecoveryAction            ├─ terminal   -> stop, show the verdict
        ├─ 'prompt' -> prompt           ├─ retry_resource -> wait, re-observe
        └─ reopen at lower rung         ├─ hold       -> keep waiting, no prompt
                                        └─ none/timeout -> today's path
```

The `none`-or-timeout fallback is not a hedge: a server that has not yet
decided must not strand a stalled viewer, and the controller may legitimately
have nothing to say. Delete the fallback only when the server can be shown to
answer every stall, which is a later slice with its own evidence.

`PlaybackPolicy.stallRecoveryAction` and `stallRecoveryTargetHeight` (~3454) are
pure and unit-tested. Keep them until the arbiter demonstrably replaces both,
then delete with their tests in the **same PR** — `-D dead-code` rejects a tree
carrying tests for a deleted helper, and splitting them cost two red CI cycles
on #663.

### 3.4 Web test lane

`tests/playback/web-control.test.js`, run by `node` directly — no browser, no
emulator, seconds not minutes. **This is why web is the platform to start
with.** `make web-check` and the layout golden cover the rest.

## 4. Apple and Android — live recon, 2026-08-31

Recced at `bc504681`. Verify line numbers at build time; both files move.
§7.4 has been rewritten from this pass — ten classifications changed and five
owners were added. What follows is the part that changes how the milestone is
built, not a restatement of the table.

### 4.1 The finding that reorders the work

**§3.2's "the observation half is done" is a web-only claim.** Neither mobile
client can receive an action at all:

| platform | reporter constructed at | `onExchange` |
|---|---|---|
| Apple | `PlaybackControlSession.swift:137`, inside `begin` (`:127`) | defaults to `{ _ in }` (`PlaybackControlReporter.swift:419`) |
| Android | `PlaybackControlSession.kt:128`, inside `begin` (`:122`) | defaults to `{}` (`PlaybackControlReporter.kt:445`) |

Both reporters already accept, validate and pace all three actions. The
parameter that would deliver one back to the player exists on both and is
defaulted away on both; the only other construction sites are the test files.
**Nothing is missing except the wire from the reporter back to the
controller** — two files and roughly thirty lines per platform.

The observation half is likewise unfinished. Both snapshot mappers consume an
`observationOverride` / `renderOverride`
(`PlaybackControlSnapshotMapper.swift:145`, `:199`;
`PlaybackControlSnapshotMapper.kt:95`, `:149`) with the right comment on it —
*a recovery path's evidence is more specific than anything derived from the
player's own state, so it wins field by field* — and **no production site sets
either**. Apple hardcodes both to `nil` at `PlayerController.swift:5269-5270`;
Android leaves the `= null` defaults and never sets `errorCode` or
`errorDetail` (`Controller.kt:1170-1201`).

So each mobile platform needs a preliminary slice — M5d and M5f in §7 — before
its first `switch` on a `ControlAction` can exist.

Apple's telemetry is not the control plane, and is easy to mistake for it:
`reportPlaybackStall` (`:3898`), `reportPlaybackFailure` (`:3777`) and
`reportPlaybackProbe` (`:3877`) all POST to `/api/v1/client-log`. They are
evidence for a human, not an exchange.

### 4.2 Apple — one function is six ledger rows

`clients/apple/Sources/PlayerController.swift`, 5313 lines.

Every stall owner funnels through `retrySameDeliveryAfterStall` (`:3156`) —
`PlaybackRecoveryMonitor`, `observeDeliveryStarvation`, and through them
`statusTask`, and it is where both bounds are consulted:

```
 statusTask :3016 ─┐
 recovery monitor ─┼──▶ retrySameDeliveryAfterStall :3156
 starvation :3039 ─┘        │
                            ├─ :3157 next(for:)  ◀── SPENDS the one attempt
                            ├─ :3164 observedStall  (rearm, not a spend)
                            ├─ :3238 boundedStallRecoveryDecision  (admits the storm budget)
                            ├─ .reopen :3183 ─▶ reopen(at:)
                            └─ .stop   :3184 ─▶ pause + verdict
```

**The ask goes before `:3157`, the function's first statement.** This is the
one placement detail worth getting right, and it is not where it looks. `:3157`
is `sameDeliveryStallRecovery.next(for:)`, which sets `attempted = true` and
returns `.stop` on every later call — it *is* the spend. `:3164`
(`observedStall`) is the opposite of a spend: it *rearms* the floor when a
minute of film has passed. An ask placed anywhere after `:3157` means a server
`hold` permanently retires the one same-delivery reopen the client had — the
exact failure a `hold` exists to avoid.

`boundedStallRecoveryDecision` (`:3238`) is `nonisolated static` and
unit-tested, and it is the Apple analogue of web's
`PlaybackPolicy.stallRecoveryAction`. **It is not pure** — it takes
`reopenStorm: inout` and calls `admit()`, which records a timestamp; its own
doc comment says so. Keep it until the arbiter demonstrably replaces it, then
delete it with its tests in the same PR.

Two more asks, each its own slice:

- the early-end `.reopen` arm (`:3543`), before the
  `recoveryReopenBudget.admit()` guard;
- `handleItemFailure` (`:3598`) — and the ask goes **before rung one**, the
  transport failover at `:3616`, not before `:3658`. The ladder's first rung
  is `retryMediaOnNextNode`, and a source verdict does not change by node:
  today an `unsupported` producer decision walks next-node → established HDR →
  transcode, guessing at retry the whole way. `terminal` short-circuits all
  three, which is the point of the slice.

The teardown a `terminal` verdict needs already exists at `:3184-3191` — the
`.stop` arm's body, starting at `player.pause()`. Take the body, not the range:
`:3183` is the tail of the `.reopen` arm and is an `await reopen(...)`.

### 4.3 Android — cheaper, with one detector that is weaker than it looks

`clients/android/app/src/main/java/tv/plurx/app/player/`.

The ask goes in `onStall` (`Controller.kt:783`), **before** the
`stallReopenBudget.canReopen()` guard at `:788` — before, not replacing it:
§1 forbids deleting a bound before its replacement is proved. **The
viewer-visible verdict path is already built**: `onError` (`:123`) is surfaced
as `playFailure` in `PlayerScreen.kt:645`, so `terminal` costs one call.

A second ask goes in `onPlayerError` (`:391`), **before `:398`** — before the
node-failover attempt, for the same reason as Apple's: `:398` both attempts the
failover and returns, so anything placed after it never sees a transport
failure at all.

Android has one genuine head start over Apple: `playbackControlPlayerChanged()`
is called at `Controller.kt:499`, on the same tick and immediately before
`sampleStall` and `onStall`. The snapshot the server holds when a stall fires
is at most one tick old.

**And one genuine weakness worth knowing before writing the PR.**
`BufferingStallTracker.sampleStall` returns a measurement only once the
playhead moves again — either when Media3 leaves the buffering state, or when
the playhead advances past `progressThresholdMs` while still buffering — and
the interval must have lasted `thresholdMs = 6_000`
(`PlaybackTelemetry.kt:264`). While the playhead is genuinely stuck it returns
`null`, forever. So `onStall` never fires during an open-ended freeze, and a
freeze in `STATE_READY` arms no baseline at all.

That is materially weaker than Apple's detector, and the control plane can only
be asked at the moments the detector reaches. **An Android freeze that never
ends stays invisible to the control plane after this milestone.** Closing it is
its own slice, and no document should describe M5g as if it were closed.

`playbackErrorAction` (`PlaybackPolicy.kt:62`) is pure and unit-tested: same
treatment as its Apple and web counterparts.

### 4.4 Build numbers, and the two gates that are not the same gate

| | current | source of truth | how to bump |
|---|---|---|---|
| Apple | 95 | `clients/apple/project.yml:18` | `make apple-build-bump` — six surfaces across four files, and a note under `docs/apple-builds/` carrying `Build: N` |
| Android | 54 | `clients/android/app/build.gradle.kts:48` | **no tool exists.** Hand-edit, plus `clients/android/README.md:23` |

The counter itself is gated by `python3 -m validation.mobile_versions`, CI job
`mobile_version`, which reads **only** `build.gradle.kts`. The *prose* copies
are a different gate: `validation/doc_versions.py`, reached through
`tests/operations/test_mobile_build_claims.py` and `make operations-check`, in
CI job **`preflight`**. An agent that bumps the counter and re-runs only
`mobile_version` goes green and then fails `preflight`.

`docs/STATUS.html` carries the Apple build under `doc_versions` too; **the
Android number there is unenforced**, which is a reason to update it by hand
rather than a reason to skip it.

`mobile_versions` compares against the **merge target**, not the branch point,
so every mobile PR merging after the first must re-bump. Four mobile slices
are planned here, so **budget three re-bumps** — two Apple, which have a tool,
and one Android, which does not.

### 4.5 Test lanes

| lane | command | CI job | runnable here? |
|---|---|---|---|
| wire conformance | `python3 -m unittest discover -s tests/validation` | `preflight` | **yes — seconds** |
| Android JVM + lint | `make android-test` | `android_jvm` | yes, via Docker |
| Apple iOS + tvOS | `make apple-test` | `apple` | no — needs `xcodebuild` |
| Android instrumented | `make android-instrumentation` | `android_device` | no — needs an emulator; flakes on `adb: device offline` |

`tests/validation/test_control_wire_conformance.py` reads both reporters by
path and pins the three action names across all four ports. Any edit to either
reporter's supported-action list or to `ClientObservation`'s wire field names
fails there, on Linux, in seconds — so run it before pushing either mobile PR
even though the slices as specified change neither.

Apple's existing coverage for `boundedStallRecoveryDecision` and
`PlaybackStallDetector` is in `clients/apple/Tests/AppleClientTests.swift`.
There is no `PlayerControllerTests`.

### 4.6 Ruling D1 — what `terminal` does to the player

§8 question 2 was open, and both mobile slices need an answer to be written at
all. **Decided 2026-08-31, in the absence of the operator; flag it if it is
wrong.**

> A `terminal` verdict arms the verdict; it does not tear the player down.
> Buffered media plays out. The moment playback can no longer continue, the
> viewer sees the server's message and a *Try again* instead of the client's
> own guess.

Three reasons, in order of weight:

1. **It is the only answer that is safe on today's fleet.** §5's gate exists
   because a client acting on a server verdict it cannot yet receive turns a
   stall into a dead player. A verdict that only *replaces the text of an
   already-failed path* cannot do that, so M5d and M5f stay genuinely
   behaviour-neutral and can land ahead of the gate.
2. **The reporter is already right.** `PlaybackControlReporter.swift:564-568`
   and `.kt:608-613` both stop reporting and deliberately leave the player
   alone, with the comment *buffer already fetched is still worth playing*.
   Tearing down on `terminal` would contradict working code for no gain — the
   viewer loses film they already have.
3. **A terminal verdict is about a recipe and a source, not about the
   viewer's intent.** Web already offers *Force transcode* from a diagnosed
   stall for exactly this case. Removing the viewer's remaining options
   because production gave up is a product regression wearing a protocol
   change's clothes.

What this rules out: any acceptance criterion of the form "an exchange
returning `terminal` stops playback". The criterion is that the *verdict shown
at the end of playback is the server's*, not the client's invented one.

## 5. Do not start before this reads non-zero

```
plurx_playback_control_vocabulary_total{complete="true",platform="…"}
```

It counts accepted exchanges from clients that declared **every** action this
server can send. Its companion:

```
plurx_playback_control_actions_suppressed_total{platform="…"}
```

counts exchanges where production was held or stopped, the server knew exactly
why, and the client was told nothing because it had not declared the action.

**Both read zero as of 2026-08-31, because the fleet has not been deployed.**
Apple build 95 and Android 54 exist and have never run on hardware; the last
recorded fleet mobile release was Android 47 · Apple 86.

Deleting a client's recovery while the fleet still runs a build that cannot
receive the replacement is how a stall becomes a dead player. `vocabulary_total`
is the check, and it is cheap: read `/metrics` on any node.

## 6. Working rules — the ones that cost time when missed

Standing constraints from the operator:

- **Never work in `~/code/*`.** Clone fresh from GitHub into the device VM's
  `$HOME`, outside `mnt/`. `git clone --shared` does not count.
- **Merge commits, never squashes.** The regression ledger addresses commits by
  id; a squash broke main once.
- **No test, lint or typecheck steps in ansible roles.** If it is merged, it
  passed.

CI gates that fail late and cost a full cycle each:

| gate | what catches people |
|---|---|
| `cargo fmt --all --check` | no rustfmt on the device VM (`noexec` mounts), so expect one red cycle and apply the gate's own diff verbatim |
| `-D dead-code` | a helper shipped before its consumer. Use `#[cfg_attr(not(test), allow(dead_code))]` — the repo's own convention, see `CleanupPolicy` — or ship it with its consumer |
| mobile release version | any change under `clients/*/src|Sources` forces a build bump, measured against **current main**, not your branch point |
| documented builds | that bump also moves `docs/STATUS.html`, both client READMEs and `docs/APPLE-CLIENT-PARITY.md` |
| `history-check` | a subject reading as a bug fix demands `validation/regressions.d` evidence; a *client* fix additionally needs a row in `tests/client-fixes.toml` |
| ownership ledger | `tests/playback/rolling-producer-owners.toml` counts by regex. `.status()` on any enum matches the process-launch shape and needs an explicit recount plus a note |

Two mistakes worth not repeating, both mine, both from this session:

1. **Patching struct literals by text match.** `hold_reason: None,` appears in
   three different structs; matching it added a field to two that do not have
   it and missed one that spells the line differently. Three red cycles. Check
   the enclosing type before editing, not after CI tells you.
2. **Reading a check list before the run starts.** One skipped check with
   nothing pending looks identical to success. Require a plausible check count
   (≥12 on this repo) before believing any verdict.

Cluster contract jobs take 15+ minutes each on 19 shared self-hosted runners
and are the real pacing constraint. Two jobs died to contention on 2026-08-30
with no test signal at all; a rebase onto current main retriggers cleanly and
keeps the branch current at the same time.

## 7. Milestones

### M5a — web defers its stall decision

`persistentWait` asks the controller before `PlaybackPolicy`. `terminal` stops
playback and shows the server's verdict; `retry_resource` waits and re-observes;
`hold` keeps waiting without a prompt. Everything else falls through to today's
path.

**Acceptance:** `node tests/playback/web-control.test.js` covers all four
branches, and a stall with the server returning `none` behaves exactly as it
does today.

### M5b — web defers its truncated-stream decision

The `endedTries` branch sends its failed observation and applies the returned
action rather than counting its own budget.

**Acceptance:** a test proving `endedTries` no longer bounds anything when the
server answers, and still does when it does not.

### M5c — delete the web budgets

`stallRecoveries`, `endedTries` and the automatic half of the fallback claim,
with their tests, in one PR.

**Acceptance:** the symbols are absent from `index.html`; `stallDiagnose` and
the viewer-intent buttons still work.

### M5d — Apple opens the return path

`PlaybackControlSession.begin` (`:127`) forwards an `onAction` handler to the
reporter's existing `onExchange` parameter (`PlaybackControlReporter.swift:419`);
`PlayerController` gains a `pendingControlObservationOverride` that
`playbackControlObservation()` reads instead of the hardcoded `nil` at
`:5269-5270`. Per ruling D1 (§4.6), `terminal` arms the verdict rather than
tearing the player down: it sets the message the *already-failed* path will
show, replacing the client's invented one.

Behaviour-neutral on a fleet that sends no action, which is every node today —
so this lands ahead of the §5 gate.

**Acceptance:** an exchange returning `none` changes nothing observable; an
exchange returning `terminal` followed by an exhausted buffer shows the
server's message rather than `playbackStoppedFailureTitle`'s generic text;
`python3 -m unittest discover -s tests/validation` passes.

### M5e — Apple defers its stall decision

The ask goes into `retrySameDeliveryAfterStall` **before `:3157`** — before
`next(for:)` spends the one same-delivery attempt (§4.2). Then, as separate
slices, the early-end `.reopen` arm (`:3543`) and `handleItemFailure` (`:3598`,
before the `:3616` node-failover rung).

Nothing is deleted. The existing path stays as the `none`-or-timeout fallback.

**Acceptance:** the four branches are covered in
`clients/apple/Tests/AppleClientTests.swift`; a stall with the server returning
`none` behaves exactly as it does today; and a `hold` leaves
`sameDeliveryStallRecovery.attempted` false, proved by a test that asserts a
subsequent real stall still reopens.

### M5f — Android opens the return path

Same two-part unlock, smaller: `onExchange` forwarded from
`PlaybackControlSession.begin` (`:122`) to the `PlaybackControlReporter.create`
call at `:128`, and an override field read by `playbackControlObservation()`.
`terminal` sets the message that the existing `onError` path will show.

**Acceptance:** as M5d, on the `android_jvm` lane.

### M5g — Android defers its stall decision

The ask goes into `onStall` **before** the `stallReopenBudget.canReopen()`
guard at `:788`, and into `onPlayerError` **before `:398`**, the node-failover
attempt (§4.3). Neither guard is removed in this slice.

**Acceptance:** as M5e. And §4.3's limit is stated in the PR body rather than
elided — this slice does not make an open-ended freeze visible to the control
plane, and must not be described as if it does.

### M5h — delete the mobile budgets

Only after `vocabulary_total` reads non-zero. Apple's
`SameDeliveryStallRecoveryState`, `StallReopenBudget`'s counting half and
`RecoveryReopenBudget`; Android's `StallReopenBudget` counting half.

**The prerequisite that is easy to miss:** on both platforms the bound and the
viewer-intent fencing sequence live in the same type — Apple's
`StallReopenBudget` and Android's `StallReopenBudget.userActionSequence`, which
`ControllerStallGuard` delegates to. **Move the fencing out first, in its own
PR**, or deleting the bound deletes fencing §7.4 says to retain.
`StallBudgetTest.kt` covers `ControllerStallGuard` and
`SessionCreateCoordinator` as well, and cannot travel with the deletion.

## 8. Open questions

1. **How long may a client wait for an action before falling back?** The
   controller's exchange is bounded at four seconds server-side, but a stalled
   viewer is already waiting. A number is needed and this document does not
   invent one.
2. ~~**Does `terminal` end playback, or offer the verdict with a Try again
   button?**~~ **Answered by ruling D1 (§4.6).** Neither: it arms the verdict
   that the already-failed path shows, buffered media plays out, and the
   viewer keeps *Try again*. Decided 2026-08-31 without the operator, because
   both mobile slices need an answer to be written at all — flag it if it is
   wrong. The plan authorises teardown but does not mandate it, and the
   reporters' existing comments already take this position.
3. **`retry_resource` while a player is starved** — the client keeps its buffer
   and re-observes, but nothing says how many times before it gives up, or
   whether that bound belongs on the client at all.
