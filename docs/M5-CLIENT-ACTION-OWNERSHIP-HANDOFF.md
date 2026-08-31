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

`producer_state` says `failed`. That single word covered **fourteen** distinct
producer decisions until 2026-08-31, which is exactly why no client could
decide anything: twelve of them are timing, process or executor facts where the
next attempt may work, and two are verdicts about the source that no retry
changes. `DeliveryView.producer_decision` now carries which one it was, and
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
| `stallDiagnose` | ~8350 | **No.** Diagnoses and shows *Try again / Force transcode / Close*. This is viewer intent — §7.4's `PlayerReopenQueue` row says retain that half |
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

## 4. Apple and Android — not yet recced

§7.4 lists the symbols. Both have a real cost web does not: Apple needs Xcode
and Android needs an emulator, so neither can be run outside CI from a Linux
box, and the Android instrumented lane flakes on `adb: device offline` roughly
one run in three.

Recon each before planning the change, the way §3 does for web. In particular
find out, for each owner, whether it is genuinely automatic or is viewer-intent
wearing a recovery-shaped name — the web table shows that assumption is wrong
at least a third of the time.

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

### M5d, M5e — Apple, Android

Recon first (§4), then the same three steps per platform.

## 8. Open questions

1. **How long may a client wait for an action before falling back?** The
   controller's exchange is bounded at four seconds server-side, but a stalled
   viewer is already waiting. A number is needed and this document does not
   invent one.
2. **Does `terminal` end playback, or offer the verdict with a Try again
   button?** The plan says it authorises teardown; it does not say teardown is
   mandatory. The kinder reading is that the player stops and the viewer is
   told why, with their place saved.
3. **`retry_resource` while a player is starved** — the client keeps its buffer
   and re-observes, but nothing says how many times before it gives up, or
   whether that bound belongs on the client at all.
