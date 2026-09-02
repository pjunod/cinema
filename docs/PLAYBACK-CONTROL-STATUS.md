# Playback control rewrite — project status

**Updated:** 2026-09-01 · **Baseline:** `main` at `1888647` (`v0.3.0`) ·
**Fleet:** all four nodes serve `v0.3.0-49-g03b4daa3`, verified off
`/api/v1/server` · **Devices:** Apple 99 and Android 56 were **installed** on
2026-08-31; the web arm of the acceptance has since passed on nuc3 and the
device arms remain unrun — see §"The first fleet run" and §"M5c is struck" ·
the tree is Android 60 · Apple 104

Companion to
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) (what
the system must become) and
[PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md)
(the ordered roadmap) — this is *where the work is right now*. Read the
tables; the chronicle below them is the running record of how each merged
item got there, kept because its failure modes recur.

## Roadmap — the ten items of the handoff §6

The order below item 6 is not negotiable. Each milestone's prerequisite is
load-bearing rather than tidy: nothing may consume an action until one client
can, the acknowledgement contract must be frozen from measured hardware
behaviour before a recipe handoff is built on it, one node must handle a
transaction before three do, and the new path must be proven before the old
one is deleted.

| # | milestone | handoff | state |
|---|---|---|---|
| 1-4 | protocol, transport, hold/resume barriers | — | merged |
| 5 | delete detached recovery loops | — | merged as #663 |
| 6 | M5 — one client action owner | [M5](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md) · [acceptance](M5-FLEET-ACCEPTANCE.md) | **finished.** M5a and M5b merged; **M5c and M5h are struck, not deferred** — the budgets they delete now bound the server's own answer. See §"M5c is struck" |
| — | M5.5 — preparation feasibility | [spike](M5.5-PREPARATION-FEASIBILITY-SPIKE.md) · [execution](M5.5-SPIKE-EXECUTION-HANDOFF.md) · [staged generations](M5.5-STAGED-GENERATIONS-HANDOFF.md) | store half **merged as [#726](https://github.com/pjunod/plurx/pull/726)**; the spike **ran 2026-09-01 on all three platforms** and is complete: web and Android `false`, **Apple `true`** after a corrective instrument pass — see §"M5.5 ran, and two thirds of it settled" |
| 7 | M6 — prepared recipe handoff | [remaining](REMAINING-ROADMAP-HANDOFF.md) §3 · [handoff](M6-IMPLEMENTATION-HANDOFF.md) | **ready to build** — M5.5 measured, three of the roadmap's open questions answered, and the interruption bound starts at 2,246 ms |
| 8 | M7 — content-analysis index | [analysis](CONTENT-ANALYSIS-INDEX-HANDOFF.md) · remainder plan in [#740](https://github.com/pjunod/plurx/pull/740) | **four of five** merged as [#700](https://github.com/pjunod/plurx/pull/700); the fifth, **subtitle windows, merged as [#742](https://github.com/pjunod/plurx/pull/742)** with readiness reporting in [#741](https://github.com/pjunod/plurx/pull/741). M7's own **M3 (seek coalescing) is built as [#754](https://github.com/pjunod/plurx/pull/754)** — see §"M3's latch, and the decision that landed it". R-M1 closes the unknown-readiness contract and carries the dated [large-MKV observation](M7-M1-LARGE-MKV-OBSERVATION.md); R-M2 remains gated on R-M1 acceptance. **M4 (burn-join) is a separate active effort**, not part of this remediation; detection is separately deferred |
| 9 | M8 — cluster handoff | [remaining](REMAINING-ROADMAP-HANDOFF.md) §4 | not started |
| 10 | M9 — cutover and deletion | [remaining](REMAINING-ROADMAP-HANDOFF.md) §5 | not started |

### M7 R-M1 acceptance needs code and an observed cache transition

Readiness source landed before its acceptance evidence. R-M1 closes both
remaining facts: an older relay preserves an unknown string unchanged, while
the web consumer treats only exact `ready` as ready; separately, a dated
local-hardware run records `warming` → `ready` and two first-half cold-cache
subtitle windows inside the 5-second publication budget.

The two facts stay separate because a serialization test cannot prove ffmpeg
publication on a large file, and a successful local extraction cannot decide
the forward-compatible wire contract. The retained measurements and their
limits are in
[M7-M1-LARGE-MKV-OBSERVATION.md](M7-M1-LARGE-MKV-OBSERVATION.md). A
tracking-only PR is not acceptance and is not required; R-M2 stays closed
until the implementation change carrying both facts is accepted.

## Slice ledger — every PR in the current push

Slices are one platform or one durable-state concern each, because the mobile
build-number gate rejects a bump computed against a commit that is not the
merge target: two client PRs in flight means the second always fails.

| slice | what it does | PR | state |
|---|---|---|---|
| docs | status page, §7.4 re-verification, Apple/Android recon, ruling D1 | [#705](https://github.com/pjunod/plurx/pull/705) | merged |
| M5a | web: `persistentWait` asks the server before deciding | [#706](https://github.com/pjunod/plurx/pull/706) | merged |
| M5.5 spike | the three-platform measurement procedure | [#707](https://github.com/pjunod/plurx/pull/707) | merged; **needs a hardware run** |
| M5d | Apple: the return path, and a verdict that outlives its reporter | [#709](https://github.com/pjunod/plurx/pull/709) | merged |
| M5f | Android: the return path, mirroring M5d | [#711](https://github.com/pjunod/plurx/pull/711) | merged |
| M5b | web: the truncated-stream owner defers too | [#712](https://github.com/pjunod/plurx/pull/712) | merged |
| M5d follow-ups | Apple: a lifetime for the verdict and the evidence | [#713](https://github.com/pjunod/plurx/pull/713) | merged |
| M5.5 recon | the staged-generations recon and plan | [#714](https://github.com/pjunod/plurx/pull/714) | merged; built as [#726](https://github.com/pjunod/plurx/pull/726) |
| M5e | Apple: the stall funnel asks before it decides | [#715](https://github.com/pjunod/plurx/pull/715) | merged |
| web bound | ruling D3 applied to the web ask | [#717](https://github.com/pjunod/plurx/pull/717) | merged |
| M5g | Android: the stall owner asks before it decides | [#718](https://github.com/pjunod/plurx/pull/718) | merged |
| ask exits | Apple: read the answer slot at every exit | [#719](https://github.com/pjunod/plurx/pull/719) | merged |
| acceptance | what a fleet run has to show | [#720](https://github.com/pjunod/plurx/pull/720) | merged |
| M5e ladder | Apple: ask before walking the compatibility ladder | [#721](https://github.com/pjunod/plurx/pull/721) | merged |
| M5g ladder | Android: skip the unchanged retry on an armed verdict | [#722](https://github.com/pjunod/plurx/pull/722) | merged |
| verdict scope | Apple: narrow #721 to the retry the verdict rules out | [#723](https://github.com/pjunod/plurx/pull/723) | merged |
| status | close out M5's buildable work | [#724](https://github.com/pjunod/plurx/pull/724) | merged |
| acceptance | refresh the roster, and state what item 3 settles | [#725](https://github.com/pjunod/plurx/pull/725) | merged |
| web P0 | the reporter's timers, which never fired in a browser | [#727](https://github.com/pjunod/plurx/pull/727) | merged |
| content index | M1-M4 of the shared analysis index — not this lane | [#700](https://github.com/pjunod/plurx/pull/700) | merged |
| fleet run 1 | what the first run settled, and what it did not | [#731](https://github.com/pjunod/plurx/pull/731) | merged |
| M5.5 store | staged generations: prepare, commit, abort | [#726](https://github.com/pjunod/plurx/pull/726) | merged |
| M5a wire | caps v2: Dolby Vision Profile 7 → 8.1 conversion in the copy session | [#716](https://github.com/pjunod/plurx/pull/716) | merged |
| status | M5a is merged | [#732](https://github.com/pjunod/plurx/pull/732) | merged |
| spike lock | catch the stranded lockfile in the fast lane, not in CI | [#733](https://github.com/pjunod/plurx/pull/733) | merged |
| browser gate | two real exchanges, in real Chromium, in CI | [#729](https://github.com/pjunod/plurx/pull/729) | merged |
| acceptance | read the commit and the builds, do not quote them | [#734](https://github.com/pjunod/plurx/pull/734) | merged |
| status | what item 8 actually delivered, and the M5.5 store half | [#735](https://github.com/pjunod/plurx/pull/735) | merged |
| release | v0.3.0: the API, the config and both schemas moved | [#736](https://github.com/pjunod/plurx/pull/736) | merged |
| prewarm | withhold a rate that has no possible numerator | [#738](https://github.com/pjunod/plurx/pull/738) | this change |

**What item 8 did not deliver.** §6 item 8 names five things: exact
intro/credits annotations, subtitle windows, force-analysis controls,
queue/current-work visibility, and instrumentation. #700 delivered four. One is
missing.

- **Subtitle windows.** Not one added line of #700 mentions subtitles, and the
  word appears once, incidentally, in the whole content-analysis handoff. This
  is **unscoped, not deliberately excluded** — the handoff's milestones are M1
  persist, M2 manual override, M3 queue, M4 operator surfaces, M5+ detection,
  and subtitle work appears in none of them, nor in its open questions. The
  gap is between the two documents, not inside either one. Client-demand-driven
  subtitle materialization is specified in
  [PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) §13.8
  and has no owner.

**A separate defect the same reading found.** Marker-destination prewarm is
*not* one of item 8's five — it is a §13.8 bullet — but its instrumentation
shipped without it. Every callsite on all three clients emits a hard-coded
`"miss"`: `crates/plurxd/src/web/index.html`, `PlayerController.swift`,
`PlayerScreen.kt`. No `"hit"` path exists in the product, so
`plurx_playback_marker_prewarm_hit_ratio` is pinned at zero permanently.
**A metric that can only read zero presents an absent feature as a broken
one**, and that costs more than silence: an operator investigating a 0% hit
rate is hunting a bug that does not exist. This one *is* deliberate on the
index side — [CONTENT-ANALYSIS-INDEX-HANDOFF.md](CONTENT-ANALYSIS-INDEX-HANDOFF.md)
§6.1 forbids that branch from touching `playback_control.rs`, and §4.5 defers
actor-side consumption to this programme. The counter is waiting for a consumer
this programme owes it.

**The gauge is no longer published until a hit is observed.** A rate with no
possible numerator is not a measurement, and the single value `0.000000` was
carrying two unrelated facts — nothing prewarms, and this node has served no
marker skip since boot. The counters still carry every miss, so nothing is
lost and the quotient stays derivable; what is withheld is the server's claim
to have measured a rate. The gauge appears the first time a hit is recorded,
which is exactly when it starts meaning something.

Two rejected approaches are worth recording as non-goals, because both were
tried here:

- **Redefining `"hit"`** as "the destination was already in the client's
  ordinary forward playback buffer". On direct play a browser buffers minutes
  ahead, so the ratio degenerates into a proxy for which transport served the
  skip — a fact the transport labels already carry — and an unbuilt feature
  starts reading as a working one. Worse than the zero, by exactly the
  argument that motivated changing it.
- **Explaining the zero in the metric's `HELP` text.** Prometheus drops `HELP`
  on remote write and federation and the query API does not return it, so the
  only reader reached is someone curling `/metrics` by hand — the reader least
  likely to be confused. A label on the trap is not a fix for it.

**What is left in M5.** Nothing buildable. M5c and M5h — deleting the web and
mobile budgets — are all that remains, and both are gated on
[M5-FLEET-ACCEPTANCE.md](M5-FLEET-ACCEPTANCE.md) rather than on any code. The
next thing that has to happen is a fleet run, and it is not a thing this side
can do from here. §3 and §4 of that document are the procedure, and §4.0 is
the web arm, which needs no device and should be settled first — it is the
only platform that can prove an exchange completes without a person holding
something, and the web reporter's fix is still unconfirmed on the fleet.

### Why Android does not await the verdict, and Apple does

The two ladder slices are not mirrors, and the difference is not an oversight.

Apple's `handleItemFailure` was already `async` before this milestone, and it
already carried the fence that makes a deferred decision safe: `openGeneration`
for a session open, `currentItem` identity for the media, and
`isChangingStream` for an open still *in flight*. Adding an `await` there costs
the ask's bound and nothing structural.

Android's `onPlayerError` is a `Player.Listener` override and cannot suspend,
so awaiting means launching the ladder on a coroutine — and Android has no
equivalent of `isChangingStream`. Four adversarial passes over that version
found, in order: a stall detector racing the ladder for the same evidence slot;
a fence that cancelled the very session create whose 404 had woken it; a fence
that stood aside for actions which re-prepare nothing, freezing the picture
with no error and no affordance for the life of the screen; and a wait that
could not tell an open which *prepared* from one which had already shown the
viewer an error, so both outcomes happened. Each fix was correct and each
exposed the next. The common root is that `openSession` reports its outcome to
nobody, so "is a re-prepare coming, and did it work" is not a question the
client can currently answer.

So Android arms rather than awaits. `reportControlEvidence` notifies the
reporter urgently, the exchange carrying the failure goes out immediately, and
the verdict it earns lands in `terminalVerdict`, where a later rung reads it —
without a coroutine, and without reordering anything.

**What this leaves owed.** Making `openSession` publish its outcome
(prepared · aborted · already reported) is the prerequisite for any deferred
decision on Android, and M6's `buffered_break_before_make` will need it too.
It is not in M5's scope and is recorded here rather than attempted.

### A terminal verdict is recipe-scoped, and both ladder slices assumed source-scoped

Found by the fifth review pass, and the more consequential of the two findings
in this section.

`terminal` is emitted only for `Unsupported` and `InvalidConfiguration`
(`playback_control.rs` `resolve_action` → `is_permanent`), and `is_permanent`
documents itself as *"whether retrying this source, **unchanged**, can ever
succeed"*. `unsupported`'s own sentence is "this source cannot be carried by
**this delivery pipeline**" — a copy-producer exit. The server treats it as
recoverable by changing the pipeline: `execute_prepublication_copy_retry` is
admitted for exactly this reason.

The compatibility ladder's last two rungs *change the recipe*. A Dolby Vision
remux and a compatibility transcode both ask the server for a different
pipeline, and `forceCompatibilityTranscode` flips `copyableVideo` to false. So
a `terminal` verdict does not rule them out — and a client that short-circuits
them deletes the rung most likely to still produce a picture, then captions the
failure with a sentence about a pipeline nobody is proposing any more.

Only `RetrySameHDRDelivery` re-prepares the identical recipe, so that is the
one rung the verdict licenses skipping. This slice is scoped to it.

**Settled on both platforms.**
[#723](https://github.com/pjunod/plurx/pull/723) narrowed Apple's, which had
merged with the short-circuit ahead of the whole ladder. Its review found the
second half of the answer: the verdict rules out the established-HDR
*reconnect*, but not whether that rung runs at all — the rung also carries the
stop that keeps an established HDR delivery from descending to SDR, and
skipping the call handed the compatibility ladder a stream it has always been
vetoed from. The gate sits inside the rung, on the reconnect only.

## The first fleet run — installation passed, acceptance did not

2026-08-31, against `943d8a9a`, Apple 99 / Android 56. `scripts/ship-physical`
completed its first end-to-end run and installed on every reachable device.
**It settled none of the rulings**, and the reason it settled none of them is
worth more than the run cost.

**Web: the control plane had never run.** The player rendered VOD HLS in about
2.9 s and its Control panel sat at *"awaiting first acceptance · exchange in
flight"* forever, because the reporter threw before exchange one:

```
TypeError: Illegal invocation
    at Reporter.drain (.../assets/playback-control.js:237:35)
```

`setTimeout` is a WindowTimers method, every browser brand-checks its receiver,
and the reporter stored it on itself and called `this.setTimer(...)`. **No web
control request has ever reached the server.** That is the whole explanation for
`vocabulary_total{platform="web"}` reading zero — not a deployment gap, a dead
runtime. Fixed in [#727](https://github.com/pjunod/plurx/pull/727).

Node does not brand-check, and every unit test injected its own timer, so the
only branch a browser takes was covered by nothing.
[#729](https://github.com/pjunod/plurx/pull/729) adds the gate that would have
caught it: the shipped module, a real browser, two real exchanges required.

**Apple and Android: the devices were locked.** Every reachable iOS device and
the Apple TV refused a foreground launch (asleep or locked); the Pixel installed
and stayed on the keyguard. No title played on either platform, so §4.1, §4.2
and §4.3 are all still unobserved.

**The old counters are not partial credit.** m6 held 4,657 Apple exchanges and
nynuc 3 Android, all `complete="false"` — incomplete older clients, which is
what that label means. They cannot settle a ruling about the full vocabulary,
and they predate the requested builds.

**What the run did prove:** `scripts/ship-physical` works end to end, and the
signed artifacts reach hardware. Also that the fleet was further ahead than this
page claimed — devices were on Apple 94 / Android 53, not 86 / 47.

**What the next run needs, beyond a deploy:** an unlocked, awake device with
auto-lock disabled. Web can be settled without one, and should be settled first.

## The gate that blocks every deletion, and what it does not block

```
plurx_playback_control_vocabulary_total{complete="true",platform="…"}
```

read **zero on all four nodes** on 2026-08-31, measured directly off
`/metrics`. It counts accepted exchanges from clients that declared every
action this server can send, and at that date the fleet had never run a build
that could receive one.

**That gate has since opened.** On 2026-09-01 the web arm of the acceptance
completed sixteen exchanges on nuc3 with zero `complete="false"`, and nuc4 has
since carried both web and Apple exchanges under induced conditions. The
paragraphs below are kept because the reasoning they record — why deletion is
gated and construction is not — is what §"M5c is struck" then applied.

**The server does emit actions.** `resolve_action` is called from
`local_control_response` (`crates/plurxd/src/http/hls.rs:3609`) on the live
control endpoint, not only from tests — a grep confined to
`playback_control.rs` finds only test call sites, because the production one
is fully qualified, and that mistake has been made once already in review.
What has never happened is a *client* completing an exchange that declares
the whole vocabulary.

**What that gates is deletion, not construction.** Removing a client's own
recovery while the installed build cannot receive the replacement turns the
next real stall into a dead player. So every slice below is additive until
the metric moves: the client gains a path that defers to the server action,
and keeps its existing path as the `none`-or-timeout fallback. The fallback
is not a hedge — a server that has not yet decided must not strand a stalled
viewer.

## M5c is struck, and this is the measurement that struck it

**Written 2026-09-01** from induced control exchanges on nuc4 at
`v0.3.0-49-g03b4daa3` and from the two client recovery handlers on `main`.
The full working lives in the agent notes as `M5C-VERDICT.md`, which is
outside this repository; the short form is here because it closes a milestone
and the evidence should not depend on a file the repository cannot show.

**What the fleet answered, while the delivery was troubled.** Actions recorded
off the live control endpoint, counters read from `/metrics` before and after:

| induced | observation sent | action returned |
|---|---|---|
| stall | `stalled` · `starved` · `network` | `hold { reason: "time" }` |
| truncated stream | `failed` · `failed` · `media` | `hold { reason: "time" }` |
| resume 30m into a fresh transcode | — | `hold { reason: "ahead" }` |
| paused, producer starting | — | `hold { reason: "demand" }` |

**No exchange returned `none` while the delivery was troubled**, which is the
question its predecessor `M5C-BLOCKED.md` §5 posed, and it is not the question
that decides the milestone.

**What it answered while the delivery was healthy.** Repeated against a quiet
node (`transcodes=0`, `readyz` 200 on all four):

| induced | observation sent | action returned |
|---|---|---|
| nothing — healthy session | — | **`none`** |
| `DELETE /api/v1/hls/{session}`, HTTP 204 | `failed` · `failed` · `media` | **`none`** |
| stall report on the same session | `stalled` · `starved` · `network` | **`none`** |

Thirteen exchanges spanned the session kill and every one returned `none`.
Across the night on nuc4: web `none` **23 → 58**, web `hold` static at **55**,
and `terminal` and `retry_resource` **never observed at all**, on any
platform.

**The first two rows are the finding.** A client reporting *starved* and a
client reporting *a broken stream* received the same answer, because
`resolve_action` never reads the client's observation — `request` is consulted
only for `accepts()` and `accepts_hold()`. The server answers its own view of
the delivery, never the client's report of it. That is sound: the server knows
the producer and the client does not. It is also why M5c cannot ship.

**The server does answer producer failures**, and the blocked note was wrong
to say otherwise. `producer_failure_reason` commits a decision off
`PRODUCER_PROGRESS_BUDGET` (10 s), `PRODUCER_STARTUP_BUDGET` (30 s), or a bare
process exit; `is_permanent` is `Unsupported | InvalidConfiguration` alone, so
an unsupported source resolves `terminal` and everything else `retry_resource`.

**Why each deletion is still refused:**

- **`endedTries` bounds the server, not the client.** `handleEnded` honours a
  `retry_resource` only while `endedTries <= CONTROL_DEFER_LIMIT`, because
  `retry_resource { after_ms }` bounds the *rate* of reconnection and nothing
  in the protocol bounds the *count*. M5b made the legacy counter the bound on
  its own replacement. `stallDeferrals` does the same for stalls.
- **`stallRecoveries` covers what the server structurally cannot see.**
  `stallRecoveryAction` is reached only after the `retry_resource` and `hold`
  branches have returned — that is, only on `none`. A wedged decoder or a bad
  link between viewer and node leaves the producer healthy, so `none` is the
  permanent answer, and the budget is the only bound.
- **The transcode fallback has no replacement.** The server's recipe swap is
  guarded on `!producer_media_published`, so it is unavailable for a remux
  that starts and then fails. For that case it builds an
  `ActionProposal { kind: "replace_failed_producer" }` — a string that appears
  exactly once in the repository, at its construction site. No handler
  serializes it; no client decodes it.

**Consequence for the roadmap.** M5 is finished at M5b. M5h inherits §1 and
§2 of the verdict unchanged and should be checked against the Apple and
Android handlers before it is scheduled rather than assumed either way. The
one remaining M5 client change is a comment correction at the truncated-stream
site, which still calls `endedTries` "a guess standing in for exactly that
answer" — now only half true.

**An operational note from the same session.** nuc4 could not serve an HLS
manifest in under twenty seconds (`20001 ms` / `0 bytes`, `55148 ms` /
`457 bytes`, `21001 ms` / `0 bytes`) and playback never reached a buffered
frame in twelve minutes across two titles. `plurx_analysis_queue_depth` shows
4 `claimed` and 18 `retry_wait` `fragment_index` jobs against 410 `failed`,
and `plurx_analysis_lifecycle_total{event="failure",reason="attempt_limit"}`
reads **564**. That is not a playback-control condition and it is not a queue
that is catching up. The node was quiet again when the healthy-session rows
above were taken, so those are not a saturation artefact. What remains
unmeasured is the unsupported-source arm: the analysis-failed files are
ordinary h264 and play, and a genuine producer refusal needs a bad source
placed on a node or a shell there, neither of which this session has. It
stays established by ruling D1 and by `is_permanent` rather than by
hardware.


## M3's latch, and the decision that landed it

**Written 2026-09-01, updated the same day.** Built as
[#754](https://github.com/pjunod/plurx/pull/754), and **not finished** — see
"What M3 still owes" at the end of this section. The section is kept because
the reasoning is the part worth inheriting, and because it records a gap the
plan had.

`SettledTarget { sequence, anchor_ms }` records the destination the client
actually wants, updated on every **accepted** snapshot — not only on a seek,
because a client that stops seeking has settled on where it is, and a latch
that only moved on `Seeking` would keep cancelling work for the position
actually being played. `supersedes` is deliberately narrower than "not
current": only a strictly later sequence supersedes, and only by naming a
different destination, because work at or after the settled sequence is either
this exchange's own or an exchange the actor has not accepted yet — cancelling
the latter would cancel the honest seek that is arriving.

One simplification the plan can absorb: §5.1's rule is **one condition, not
two**. The validator at `playback_control.rs:220` already rejects `Seeking`
without a valid `seek_target_ms` and a target without `Seeking`, so the
existing `buffer_anchor_ms()` *is* the settled-target expression.

**The gap.** §5.1 keys expensive production by "the control sequence that
requested it", and neither consumer receives one. `CreateSession`
(`http/hls.rs:530`) carries `playback_id` and `start` and no sequence; the M2
window extraction is reached from a per-segment GET. Ordering by arrival
instead is not a smaller version of the same rule — it inverts in the one case
that matters:

> A create for the new target arrives before its own snapshot. The latch still
> holds the previous target, the anchors differ, and the honest seek is
> skipped as obsolete.

A debounce would paper over that and §5.1 already rules it out, correctly: it
trades the storm for added latency on every honest seek. The sequence exists —
it is simply not on the wire.

**The decision, and it was one line of protocol.** Paul chose the additive
field: `control_sequence: Option<u64>` on `CreateSession`, absent meaning
today's behaviour — the pattern M5a used throughout. The alternative, moving
the restart behind the control exchange, stays available and is really M6's
design; if that happens the field simply stops being sent. `hls::create` now
consults the latch through the existing lease snapshot and refuses a
superseded restart with a typed 409 before anything is spawned.

**One thing the storm test found.** The server already refuses exchanges
faster than its own 250 ms cadence, returning `RateLimited`. That
independently bounds how many restarts a single storm can ask for, and the
test spaces its seeks above the floor so it measures the latch rather than the
rate limiter.

Worth knowing for urgency: the client already tokens its own side
(`_seekToken`, `index.html:3377`, `:3426`), so a browser storm does not
produce twenty attached players. What it produces is up to twenty session
creations the server honours. The cost is server-side waste, not client
confusion.


### M3 is closed

Both gaps this section recorded are shut, in
[#785](https://github.com/pjunod/plurx/pull/785).

**The refusal is tested.** `hls::create` cannot be reached from a test — it
needs a store, a transcode manager and an authenticated user — so the decision
lives in one function and the handler has no other way to express it. Seven
tests pin it, and the ones worth knowing are the negatives: an absent sequence
and an absent settled target both **proceed**, because absent means *do the
work*; equal and later sequences **proceed**, because a create can reach the
server before its own snapshot and refusing the honest seek is the failure this
milestone exists to prevent; and a malformed start reads as the head, which can
only make a restart look *less* superseded — a bad body must not be able to
cancel work the viewer wants.

**Every client orders its restarts.** Apple and Android now send
`control_sequence` alongside web. Each sends the reporter's own counter rather
than the accepted sequence: a create can outrun the snapshot that justifies it,
and a client reporting a *higher* sequence can only look less superseded, which
is the safe direction. Apple 108, Android 64.

## M5.5 ran, and two thirds of it settled

**Run 2026-09-01** on web and Android; **Apple produced nothing**. The full
validation is `M5.5-RESULTS-VERDICT.md` in the agent notes. The run passes
every acceptance check in the spike's §6, and twice declined a number it could
have got away with — the Chromium codec/HDR row was discarded rather than
launder a malformed asset into a platform result, and no Apple number was
invented rather than reuse the production tvOS profile over installed build
105.

**The decision is to keep `dual_player_preparation` false**, which is the
outcome [the spike](M5.5-PREPARATION-FEASIBILITY-SPIKE.md) §4 anticipated: a
measured `false` selects that platform's fallback and stops the UI calling
that path seamless. It is not a failure.

| platform | dual preparation | why |
|---|---|---|
| web | `false` | Safari same-codec reached 20 consecutive; codec/HDR only 13/20 |
| Android | `false` platform-wide | both phones passed both cases; the tunneled Google TV passed codec/HDR and failed same-codec 0/3 |
| Apple | **`true`** | iPhone 17 Pro Max and Bedroom Apple TV 4K each 20/20 on both cases; a first instrument set was rejected and a corrective pass repaired it — see below |

### The result is conditional, and the capability cannot say so

Every platform came back recipe- and device-dependent. A bare
`dual_player_preparation` boolean cannot express *yes for this recipe on this
device*, so a correct `false` throws the finding away. That is a **protocol**
observation rather than a client one — the field is frozen in v1, so a
narrower capability keyed by device and recipe is a change M6 must decide on
deliberately rather than discover.

### The Google TV inversion is the finding worth chasing

It passed the **harder** case and failed the easier one, which is backwards
from the spike's stated expectation. Two successor-prime timeouts and one
`ERROR_CODE_AUDIO_TRACK_WRITE_FAILED`, with tunneling on. Two *identical*
tunneled pipelines plausibly contend for one decoder or audio track where two
*different* codecs get distinct instances — which would make the constraint
"one tunneled pipeline per codec" rather than "this device cannot prepare".

That matters because a resolution change is same-codec, and same-codec is
M6's common case: the exotic case works on that device and the ordinary one
does not. Three attempts justify withholding the capability; they do not
diagnose it. One targeted re-run — same-codec dual prime, tunneling forced
off — would name the constraint exactly.

### What is now measured, and what is still owed

The fallback interruption is measured on two platforms, which answers the
roadmap's open question about an acceptable `buffered_break_before_make`
bound for them: web/Safari 271–2,246 ms (mean 1,121), Android/Google TV
353–766 ms (mean 471). **Worst observed is 2,246 ms**, and that is the honest
starting point.

Still owed from the run: how the throwaway switch differed from M6's (the
spike asks for it, and a runway measured against a switch M6 will not use is
wrong in a way nobody can see later), and the prime-window duration — network
duplication is reported as volume, so the rate cannot be derived, and rate is
the only reason that field exists.

### Apple is `true`, and the first instrument set was rejected to get there

Both required devices completed 20/20 on both cases with zero predecessor or
post-commit stalls. **The first instrument set was not accepted** — four of the
seven fields did not behave like measurements, and the asymmetry was
deliberate: web and Android's `false` is what the literals already say, so
accepting it costs nothing, while Apple's `true` authorises the server to prime
a second pipeline on a viewer's Apple TV.

What was wrong, and what the corrective pass did about it:

| defect | corrective |
|---|---|
| `buffered through` flat at exactly 12,000 / 12,010 ms across eighty acknowledgements on two devices — twice the successor origin offset, i.e. the successor buffering the whole fixture | 132-second fixtures; runway now varies 16,607–17,835 ms and reads `full at ack: false` |
| first-frame 1–4 ms, with the TV's two case means identical to two decimal places — no resolution at the scale measured, and `hasNewPixelBuffer` can be satisfied by a pre-commit frame | the copied pixel buffer's PTS is required to map to film time at or beyond the commit boundary, and is reported beside the latency |
| network duplication byte-identical across two different devices (575.96 Mbit each) — derived from access-log bytes, not counted | response-body bytes counted by the 80 Mbit/s proxy under unique device/trial/pipeline URLs; values now vary per trial and per device |
| peak memory measured the app while AVFoundation decodes in `mediaserverd` | declared **unanswered**, with the reason — the available tooling exposes no resident memory for `mediaplaybackd` / `videocodecd` |

**The repair is verifiable in the numbers rather than asserted.** First-frame
now *separates the two cases* in the direction physics requires — Apple TV
same-codec 21–239 ms against codec/HDR 230–327 ms; iPhone 24–160 ms against
163–279 ms. The old instrument returned the same value for both, which is what
proved it was not measuring the switch.

Three corrective pilots were rejected before the accepted rows and their
receipts retained: an exactly tight predecessor-boundary timeout, a run where
the original low-bitrate fixture still hit **AVPlayer's 53.889-second
full-buffer cap**, and a 30 Mbit/s H.264 pilot that failed item admission. That
cap is what the flat 12,000 ms was hiding.

**Two things stay unanswered on Apple, by the platform rather than by the run.**
Hardware decoder instances: AVFoundation exposes no public identity, so the
reported `2` is a logical `AVPlayer` pipeline count and **plan §5.3's
one-encoder-slot question remains open exactly where the 2160p pressure lives**.
And system-wide memory, per the table above.

**One residual worth carrying into M6.** The 20/20 commit proof ran on the
short fixture; the validated instruments ran on the 132-second one. Nobody has
twenty consecutive commits *on the corrected fixture* — the corrective pass is
four trials per device and says so. Memory is the field that would catch
accumulation across twenty trials, and it is unanswered at both fixture sizes.
Neither blocks the capability, which M6 must read rather than assume, but M6
should re-confirm on a realistic runway before it ships a behaviour change to
viewers.

Two disclosed compromises, weighed: the tvOS harness ran under the production
bundle `tv.plurx.app` because the profile was exact-bundle only — deliberate,
with an exact build-105 restore artifact secured first and build 105 restored
and verified after, and bundle identity does not change decode behaviour. And
the original tvOS receipt is reconstructed from console output; the corrective
receipt was written directly by the metering server and is not.

The hold was worth running and the fix was real. A first physical measurement
that needs four instruments tightened on the hardest platform, finds a
53.889-second buffer cap while doing it, and retains its rejected pilots is a
good run, not a poor one.

## Open decisions

Carried from the M5 handoff §8 and the remaining-roadmap handoff §8. None
blocks the slices now in flight; each shapes M6.

1. How long may a client wait for an action before falling back to its own
   behaviour?
2. ~~Does `terminal` end playback outright, or offer the verdict with a *Try
   again*?~~ **Answered** by ruling D1, M5 handoff §4.6 — it arms the verdict
   rather than tearing the player down. Decided without the operator.
3. Does a retry bound belong on the client at all?
4. Is a bounded admission overcommit proven safe on any of the fleet's
   hardware? The protocol plan's §5.3 assumes one exists as its first
   fallback (reached via [remaining](REMAINING-ROADMAP-HANDOFF.md) §3.2); if none does, the
   one-encoder-slot path is the common case rather than the exception and
   M6's shape changes.
5. ~~How long may a client wait for an action?~~ **Answered by ruling D3.**
   1.5 s, extended once to at most 3 s when an exchange was already in flight.
   The fallback is the branch the entire fleet takes, so the bound is added to
   every real stall on every device; a server that cannot answer inside a
   second and a half is a server whose answer is not worth more frozen picture
   than the recovery it would have replaced. Web's 6 s/12 s predates this and
   should be brought down to match — it is the one place the three platforms
   now disagree. Decided without the operator.
6. What interruption bound is acceptable for `buffered_break_before_make`?
   Its acceptance criterion is "within its measured interruption bound", and
   nobody has measured or chosen one.
7. Should the two ladder slices short-circuit anything but the unchanged
   retry? **Answered: no** — a `terminal` verdict is recipe-scoped, see above.
   Android is scoped to `RetrySameHDRDelivery`, Apple to the established-HDR
   reconnect. Decided without the operator, and the one to look at first: it
   is a claim about what the server means by `is_permanent`, and if that
   reading is wrong then both slices are scoped too narrowly.
8. Should Android's session opens publish an outcome, so a client decision can
   be deferred there at all? Decided *not* to attempt it inside M5 — see
   §"Why Android does not await the verdict" above. Decided without the
   operator, and the one worth a second look: it is the difference between the
   two platforms' ladder slices, and M6 needs it either way.

## Chronicle


PR #641's one permitted broad local `make unit` invocation has already run and
must not be repeated for that merged cut. The `plurxd` target reported 1,243
passed, 55 failed, and three ignored tests. One directly affected actor test
found a real retry-successor deadline regression and is fixed; all four exact
repaired successor tests pass. The other 54 `plurxd` failures are sandbox
environment failures at bind, mDNS/listener, or macOS `ps` observation seams.
The `plurx-core` cluster failures likewise stop at sandbox-denied bind/listener
setup rather than product assertions. Non-test gates are green: formatting,
lint, validation lint (23 points / 27 checks), history
(1,121/769/663/80/78), 123
operations contracts, 52 benchmark checks, web policy, and diff inspection.
Unrestricted `make cluster-check` also passed its vendor recovery, 67-test
replicated Store contract, real membership/failover/topology drills, seven
daemon activation tests, and two cluster activity tests. Three immutable
final-head reviews approved `e64e8c1` with no actionable P0-P3, hosted run
`33243486511` passed every required job, and #641 merged. A later accidental
duplicate invocation stopped before the cluster workload because macOS
platform certificate loading reported no available keychain; it reached no
product assertion and is not being retried locally.

For historical #636 validation, its one allowed full local unit invocation was
consumed before tests while compiling Store code. The directly affected rerun
then executed 641 tests:
606 passed initially, five deterministic inventory/high-water failures passed
after exact repairs, and all 30 socket fixtures passed by exact name with
unrestricted loopback binds. The aggregate cluster gate found two stale
historical migration fixtures; `411bf818` reconstructs their exact v10-v12
schemas and `4d38a084` proves the v14 acknowledgement objects and v16/v17
columns at every completed upgrade. Both failed names pass by exact name.
The repaired tip then passed the activation Store contract, five exact daemon
activation/replay tests, two SQLite migration tests, two Hiqlite schema tests,
and the exact placeholder-order invariant. The first required cluster run
passed every vendor WAL/Hiqlite probe and 61/67 replicated Store contracts;
the six failures all identified first-appearance placeholder corruption in
Hiqlite media-session SQL. `4da3bbde` repaired the four reached statement
shapes plus one latent Abandon binding, and all six failed three-voter names
passed on exact rerun. The complete cluster harness, seven real-daemon activation tests,
and two Activity tests pass. Formatting, workspace Clippy, validation catalog,
history, 123 operations contracts, 52 benchmark checks, and the web policy
gate also pass. Every local result is therefore accounted green without a
second manual/local broad-suite run. Hosted run `33226975369` stopped in fast preflight on
sixteen stale ownership-inventory counts before every downstream job. After
unanimous review, the two exact failed methods pass 2/2, `make
validation-lint` passes, and the complete hosted-equivalent Python validation
catalog passes 68/68. The PR body pins each immutable review candidate,
avoiding an impossible self-reference from a commit to its own hash.
Hosted rerun `33227941555` then stopped solely because the docs-only candidate
subject matched the corrective-history classifier. Unanimously reviewed mapping
`9471d129` records that `9ee007a8` changed documentation only; the exact
`make history-check` rerun passes with 1,100 corrective commits accounted.
Hosted run `33228540108` then passed validation scope, mobile version, fast
policy/contract preflight, WAL recovery, daemon contracts, and the complete
replicated Store/topology lane. Its final fast-Rust job exposed two stale Core
contracts, two nondeterministic fixture assumptions, and two playlist tests
whose obsolete barrier wait prevented completion; the job was cancelled at
its 30-minute bound before libtest could print the latter target's summary.
The reviewed repair now classifies all 48 SQLite transaction owners, checks
the actual read-only Hiqlite replay shape, rendezvouses playlist races at the
actor observation boundary, exercises software admission without a nonexistent
media source, publishes VOD fixture claims through the complete three-phase
contract, and isolates fair terminal eviction from paused-time Store probes.
All six exact affected tests pass. Three independent repair reviews report no
remaining P0–P3 finding; only immutable-tip review, the hosted rerun, and merge
remain.

Hosted rerun `33230939191` subsequently passed every selected substantive lane
other than fast Rust, including the complete replicated Store/topology lane,
but fast Rust reported
eight failures and one cleanup hang. Seven failures and the hang were stale
fixture ownership/timing assumptions: the repair separates scratch from app
state, drives the real serving authority, observes the correct rolling or VOD
registry, waits for detached response projection, refreshes frozen subtitle
metadata, and rendezvouses exact waiters rather than sleeping. The ninth
failure found a real close-versus-successful-acknowledgement race in delivered
byte accounting; successful acknowledgements now win simultaneous receiver
closure in both VOD and rolling pumps, while canceled acknowledgements preserve
deadline and disconnect classification. All nine names pass by exact rerun.
Two adversarial lanes approve the complete repair; no second broad local suite
was run. Formatting, package Clippy, catalog, history, and exact-name gates are
green. Corrected tip `cdc27aa8` passed two exact-head reviews, was pushed, and
started the next hosted run.

Hosted run `33234021296` stopped in preflight before broad Rust or cluster jobs
because three whole-module structural sentinels had not counted the repair's
test-only synchronization. Independent reviews proved exact net additions of
one joined waiter task, nine bounded fixture timers, and six Barrier `wait`
tokens conservatively matched by the process-lifecycle regex. No production
task, timer, process action, watchdog, or recovery owner changed. Repair
`c94cdcf3` preserves every regex, whole-module scan, and equality assertion;
the exact failed validation method passes, and `4ad4a4da` maps the evidence.
Hosted run `33234420211` then passed validation scope, mobile-version policy,
contract preflight, WAL recovery, every daemon contract, and the complete
replicated Store/topology lane. Fast Rust ran its hosted broad suite to
completion: 1,281 passed, three were ignored, and seven stale fixture
assumptions failed. Repair `4b76a34e` now compares actor delivery at one fixed
coordinate, keeps read-only playlist observation separate from response
publication, commits retention through the exact playlist owner, captures the
idle baseline before status polling, exercises hardware capacity through the
actual admission primitive, and tests duplicate-request coalescing through the
claim state machine. All seven failed names pass by exact rerun. Package
Clippy, formatting, diff inspection, the exact ownership inventory,
validation lint, and history pass; `7dc30a42` records the regression evidence.
Three source reviews and an exact mapped-head review approve the repair with no
actionable P0–P3 finding. No second manual/local broad unit suite was run.
Final exact head `d1b117bf2bddba600c659f2b2354ea50ee10830a`
then passed hosted run `33236731526` with every required job green. PR #636
merged as `a48884906da351ca0c72dc99c227aa169911d089`.

The post-rejection work is assembled in three tracks:

- HTTP/routing/relay now carries exact owner authority through VOD status,
  durable local/remote/terminal classification, status rerouting, release,
  remote abort, resurrection, range/`If-Range`, and EOF. Request preparation
  retains one inherited deadline; an admitted relay body owns a separate
  bounded streaming lifecycle. DELETE elects one cancellation-safe settlement
  token, duplicates join its exact result, commit-unknown End retries through a
  sharded bounded registry, and terminal authority is published before
  detached process reclamation.
- Rolling authority/settlement now makes Terminal admission irreversible once
  queued, finds the exact Session across durable-ID adoption, releases the
  global registry before actor/process settlement, publishes failure before
  kill/reap, and retains process admission plus scratch until confirmed reap.
  Detached retirement and scratch owners survive caller cancellation.
- VOD uses per-rendition single-flight ownership for slow builds, bounded and
  confirmed-reap missing-init regeneration, cancellation-safe dormant purge,
  immediate heavyweight terminal-graph compaction, and bounded compact `410`
  replay followed by fail-closed durable eviction.

The pre-rebase implementation frozen through rebased commit `611d60cf` joins
those tracks at their
shared publication boundary:

- replacement is make-before-break: the provisional successor activates by
  Store CAS before the authoritative predecessor is projected terminal, while
  persisted `publication_ready_at_ms` is an explicit three-state fence. Only
  `0` is publishable; activation writes an indefinitely blocked sentinel, the
  first exact post-commit observer arms a fresh 372-second not-before value,
  and exact acknowledgement or an exact elapsed-boundary Store proof clears
  it. Routing and idempotent replay never infer readiness from wall time;
- Store rows now retain the first `terminal_reason` across public release,
  supersession, admin/revoke paths, stale-owner cleanup, and node removal.
  SQLite and Hiqlite migrations, route queries, replicated dump/import, and
  old-schema import defaults carry both new fields;
- release is explicitly two phase: install a cause-neutral local publication
  fence, await the uncancelled first-writer Store End, cache its exact terminal
  route or definitive absence, then project that durable cause to the exact
  owner. A shared `204` is published only after replay-visible proof and owner
  acknowledgement, or after the finite admitted-body safety boundary. Ended
  rows reuse `publication_ready_at_ms` as a durable projection ledger:
  sentinel means the post-End boundary is not armed, finite means the one
  restart-stable fallback is pending, and `0` means exact acknowledgement or
  exact boundary proof was persisted. A restart never mints a second window;
- attempt-derived bodyless status and typed failure validate the exact
  Session/attachment and rolling attempt without renewing demand or advancing
  delivery. Streamed body authorization commits only after all advertised
  bytes have been read and accepted by the bounded HTTP-body channel; a fully
  buffered body commits only after exact preparation immediately before
  exposure and is then wrapped by the same absolute body deadline.
  Cancellation, short read, storage error, and abandonment before the final
  streamed chunk is accepted are non-commits; and
- every admitted local or relayed media body has a 300-second absolute
  lifetime and a 30-second upstream or downstream no-progress bound. Driven
  local bodies retain at most one unacknowledged chunk and account/commit only
  after the public Body accepts that chunk; the relay pump retains two and all
  public bodies discard queued bytes after a terminal or absolute deadline.

The unchanged pre-rebase content was locally validated. Rebased runtime head
`b1eeba78`, assertion/scanner head `2afa1bba`, compile/Clippy heads through
`77e292d1`, and historical-schema head `4d38a084` retain those earlier
adversarial approvals; at that point the serving-authority source repair was
under a fresh exact-head review.
The one allowed full local unit invocation has been consumed; every failed or
directly affected name now passes, and no second manual/local broad unit target
will run. Required hosted workflows still execute their normal broad gates.
PR #626's runtime commits
`c04898e2`, `732d3442`, `91486148`, `ba3a504d`, and `4d0a0c0f` add a
behavior-neutral, one-slot immutable actor decision, a non-consuming poll
command, and one bounded passive executor inbox/task. Root static review
repaired a mailbox self-retention cycle before the first commit. Adversarial
rounds 1 through 3 found timer-only terminal wake, executor-loss visibility,
terminal-cause projection, compile/lint, test-scheduling, actor-loss
classification, terminal/receiver-loss races, stale executor-state overwrite,
and a four-variant poll-contract violation. The final defensive pass also made
the first terminal-or-lost settlement win in either order. The runtime head
received unanimous exact-head approval at `981ebb51`. The focused Rust suite
then passed 85/85; the ownership inventory exposed six stale normalized
counts/anchors and one warnings-as-errors risk. `73cd33e9` fixed the normalized
counts and warning; the rerun cleared five failures and proved the remaining
entrypoint row was invalid because that table is deliberately source-local to
`transcode.rs`. `9d100a04` removes the misplaced row while whole-module
sentinels continue to cover the transport. Targeted review approved that
repair and the inventory passed 7/7. `make check` then reached the history gate
and stopped because four corrective mapping commits were not themselves named
by their existing evidence records. After that reviewed repair, the rerun
passed catalog, history, 121 validation tests, and 52 additional tests, then
Clippy identified the intentionally retained strong transport owner as unread
outside tests. `f463d4d4` documents and allows that one lifetime-owner field.
The repair received targeted approval, the corrected handoff received final
static approval, and the final documentation corrections received exact-head
approval at `1ead3944`. `make check` and `make cluster-check` passed. Hosted
run `33127652859` then passed every selected job and the aggregate PR
validation gate on that reviewed implementation/status head. Final exact head
`62a4f535` passed hosted run `33129200705`; PR #626 merged as `9063bb1e`.
PR #624 merged as `dba35f98` after unanimous exact-head review, green local
`make check` and `make cluster-check`, and hosted run `33118301414`. Its bounded
shared command/producer sequencing, publication-time ordering, stale-exit
fencing, observation-only actor projection, and command/deadline metrics are
now part of the validated baseline. PR #636 subsequently moved
prepublication transcode recovery and response settlement into that actor and
executor. PR #641 moved published-transcode lifetime into the same owner and
merged it into `main`. PR #642 then moved copy recovery into it and merged as
`062530d2`, and PR #663 re-anchored the replacement tests, closing handoff
item 5. No compatibility recovery owner remains on `main`.
The detailed M4 contract is in
[`PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md`](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).
The resumable execution state is in
[`PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md`](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md).
**Source of truth:** this page tracks delivery; the design and acceptance
contracts remain in
[`PLAYBACK-CONTROL-PROTOCOL-PLAN.md`](PLAYBACK-CONTROL-PROTOCOL-PLAN.md).

This is the canonical done-versus-left ledger for the playback-control
rewrite. Update it in every implementation PR. A feature listed as partial is
not being counted as complete merely because its foundation has landed.

## At a glance

| Workstream | State | Shipped result | Still required |
|---|---|---|---|
| Design and adversarial review | **Complete** | End-to-end protocol, actor, replacement, cluster, index, observability, and watchdog-deletion contracts | Re-review each implementation PR against the contract |
| M1 — typed control plane | **Complete** | Strict v1 messages, capability route, sequence and owner fencing, bounded cluster relay, default-off advertisement, metrics | Actions are no longer disabled — the server emits them and all three clients consume them — but the advertisement is still default-off behind `playback.control_protocol_v1`, and turning it on is M9's |
| M2 — passive clients | **Partial** | Web, Apple, and Android all report demand, playhead, contiguous runway, render state, selections, capabilities, and recovery evidence; Apple and Android both retry through an alternate ingress (`retryMediaOnNextNode`) | Physical evidence for that retry, and the §13.2 playback-lab run that joins each legacy recovery to the control snapshot before it. Web cannot claim alternate-ingress retry at all without a CORS/auth contract — see [M2-WEB](PLAYBACK-CONTROL-PROTOCOL-M2-WEB.md) — so this row stays partial on measurement, not on missing native code |
| M3 — actor and explicit lease | **Complete** | Rolling generation has one bounded actor for control fencing, explicit demand/pacing, renewal, expiry claim, typed End/authority-fence ownership, durable terminal replay, response commit ownership, the attempt-fenced delivery ledger, nonblocking progress/exact-exit observations, and exhaustive event-order evidence | Nothing — M4 moved recovery decisions through that owner and merged |
| M4 — server watchdog removal | **Complete** | #636 merged production startup recovery; #641 merged actor-managed published lifetime, frozen frontier, typed `producer_ended`, and removal of its compatibility watcher; #642 merged actor-owned copy lifetime and removed the copy watchdog, election, request-side exit inference, and copy-owned replacement policy; #663 re-anchored the replacement tests and closed handoff item 5 | Nothing — later lifecycle/observability cleanup is a separate slice |
| M5 — one client action owner | **Complete in source** | Web, Apple, and Android each ask the server before recovering; a terminal verdict is recipe-scoped, so it gates only the rung that re-prepares the identical recipe | A fleet acceptance run, which is what the budget deletions (M5c, M5h) are gated on |
| M5.5/M6 — prepared handoff and Auto | **Store half merged** | Staged generations: prepare, commit, and abort as compare-and-swap over a preparation ledger, on both backends ([#726](https://github.com/pjunod/plurx/pull/726)) | The feasibility spike, which needs physical devices; M6's transactional resolution of bitrate, codec, HDR/Dolby Vision, audio, subtitle, and node changes may not start without its numbers |
| M7 — semantic indexes/subtitles | **Four of five** | The fragment index, plus ([#700](https://github.com/pjunod/plurx/pull/700)) exact intro/credits annotations with provenance and confidence, manual overrides, force-analysis controls, and queue/current-work visibility with its instrumentation | Subtitle windows; and outside item 8, seek coalescing and a marker prewarm consumer that can report a hit; detection itself is separately gated |
| M8 — cluster handoff | **Not started** | Control relay and owner fencing exist from M1 | Planned drain, VOD resurrection, rolling successor failover, compatibility-takeover retirement |
| M9 — cutover/deletion | **Not started** | — | Mixed-fleet evidence, defaults on, compatibility engine and `/status` polling deleted, physical matrix green |

## Merged evidence

**This table stops at #636, and nothing replaced it.** The slice ledger above
begins at #705. Roughly eighteen playback-control PRs merged between them and
are recorded in neither table — #641, #642, #656 (carrying #652, #654, #655),
#663, the two M2 passive reporters #667 and #668, the wire conformance and
telemetry slices #674, #686, #690, #692, #695, #696, #702, #703, and the M5
handoff #704. Most appear nowhere in this file at all, including the two that
built the M2 milestone this table's own rows credit. #700, #716, and #726 are
in the ledger.

Neither table is an index, in other words, and this one is kept only for the
exact heads and hosted run ids it carries. **Trust the roadmap table over
either of them**, and `git log --merges` over all three.

| PR | Merged result | Verification at merge |
|---|---|---|
| [#600](https://github.com/pjunod/plurx/pull/600) | Detailed explicit playback-control design and independent adversarial finding ledger | Documentation review complete |
| [#602](https://github.com/pjunod/plurx/pull/602) | M1 fenced, behavior-neutral control plane and mutating cluster relay | Adversarial review, local gates, and hosted CI green |
| [#603](https://github.com/pjunod/plurx/pull/603) | Content-addressed VOD fragment-index work shared across cluster voters | Adversarial review, local gates, and hosted CI green |
| [#605](https://github.com/pjunod/plurx/pull/605) | M2 passive web control reporter and joined recovery evidence | Adversarial review, local gates, and hosted CI green |
| [#606](https://github.com/pjunod/plurx/pull/606) | Durable force-analysis requests, bounded queue/status API, Activity status UI | Merged with three red hosted jobs subsequently repaired by #610 |
| [#610](https://github.com/pjunod/plurx/pull/610) | Fixed all three #606 merge-gate failures and added the missing ops contract test | Adversarial review, local gates, and hosted CI green |
| [#611](https://github.com/pjunod/plurx/pull/611) | M3a bounded rolling lease actor and atomic expiry/retirement ownership | Adversarial approval at `dddc8d24`; full local gate and all hosted jobs green |
| [#613](https://github.com/pjunod/plurx/pull/613) | Preserved an empty Dolby Vision `hvcC` record when copying parameter sets upstream | Adversarial review, local gates, and hosted CI green |
| [#614](https://github.com/pjunod/plurx/pull/614) | M3b explicit demand lease, response-commit ownership, demand-based pacing, and operator instrumentation | Exact-head adversarial approval, full local gate, and all required hosted jobs green |
| [#615](https://github.com/pjunod/plurx/pull/615) | M3c1 actor-owned, exact-attempt publication/fetch ledger and fenced producer installation | Exact-head adversarial approval at `7dffa5f3`; full local gate and every required hosted job green; merged as `46c08439` |
| [#616](https://github.com/pjunod/plurx/pull/616) | M3c2 constant-space producer progress/exit ingress, exact-attempt actor facts, and one cancel-safe process supervisor | Exact-head adversarial approval at `4b818d39`; 12 focused tests, 1,927 full-workspace tests, every local gate, the long cluster gate, and all required hosted jobs green; merged as `8c6ccdf7` |
| [#617](https://github.com/pjunod/plurx/pull/617) | M3c3 typed actor-owned End, authority fence, and lease expiry; atomic durable terminal acknowledgement and exact replay; cancellation-safe settlement and exhaustive event ordering | Exact-head adversarial approval at `6f127463`; `make check`, `make cluster-check`, and every hosted job green; merged as `9cd16d05` |
| [#618](https://github.com/pjunod/plurx/pull/618) | M4 watchdog-removal ownership, deadline ordering, executor, cleanup, process-capacity, and post-publication proposal contract | Seven exact-head adversarial passes resolved 31 findings; final approval at `96799e60`; `make check`, `make cluster-check`, and hosted PR gate green; merged as `f9cef83b` |
| [#619](https://github.com/pjunod/plurx/pull/619) | Constant-space cutoff-safe producer progress coverage plus checked whole-module legacy owner/task/timer/process inventory | Thirteen exact-head reviews; final approval at `11315987`; focused tests, `make check`, `make cluster-check`, and hosted PR gate green; merged as `48ea494c` |
| [#621](https://github.com/pjunod/plurx/pull/621) | Passive producer deadline, cutoff-safe ingress/fencing, and process-flow barriers | Ten exact-head rounds ended unanimously **APPROVED**; hosted run `33109297301` green; merged as `8e331672` |
| [#624](https://github.com/pjunod/plurx/pull/624) | Bounded shared actor-command/producer sequencing and observation-only operational projection | Formal runtime round 5 unanimously approved; `make check`, `make cluster-check`, and hosted run `33118301414` green; merged as `dba35f98` |
| [#626](https://github.com/pjunod/plurx/pull/626) | Immutable actor decision slot, non-consuming poll contract, and bounded passive executor transport | Exact-head adversarial approval; focused Rust 85/85, ownership 7/7, `make check`, `make cluster-check`, and hosted run `33129200705` green; merged as `9063bb1e` |
| [#636](https://github.com/pjunod/plurx/pull/636) | Actor-owned prepublication recovery, exact response settlement, three-phase cluster activation, and durable publication/terminal ownership | Exact head `d1b117bf2bddba600c659f2b2354ea50ee10830a`; hosted run `33236731526` fully green; merged as `a48884906da351ca0c72dc99c227aa169911d089` |

## M4 watchdog removal — merged, kept as the record

**This section is the slice's record as it was written while the work was in
flight, and it is deliberately not rewritten.** M4 is complete: #642 merged as
`062530d2` and #663 closed handoff item 5 behind it. Read the whole section as
history, not as a queue — it is kept because its failure modes recur and the
reasoning is worth more than the tense. Two things need saying before it, both
of which the tense hides:

- **"the active branch" names two different branches, once each.** The
  occurrence that names `codex/playback-control-m4-copy-lifetime` in its own
  sentence is #642's. The other one — in §"Merged baseline and active-cut
  watchdog disposition", beside a sentence about actor-managed transcode
  published lifetime — is **#641's branch**, which merged to `main` first as
  `32af5fa9`. The section says elsewhere that #641 is merged, so it
  contradicts itself on that one; #641 is merged.
- **Every "present on merged `main`" cell below is now false.** Those cells
  were true when written and describe symbols that no longer exist. The
  removal is checkable rather than asserted: `SOFTWARE_GRACE`,
  `WATCHDOG_POLL`, `watchdog_active`, `begin_copy_child_replacement`,
  `downgrade_one_step`, and `FIRST_SEGMENT_GRACE` now have zero occurrences
  anywhere under `crates/`.

PR #618 merged the complete M4 implementation contract at `f9cef83b`, after
seven adversarial passes and green local, cluster, and hosted gates. The actor
already owns explicit End, authority fence, and exact lease expiry; this slice
starts moving producer recovery evidence through that same owner.

PR #621 merged the passive deadline and cutoff-safe ingress foundation as
`8e331672`. PR #624 then merged bounded shared command/producer sequencing,
publication-time ordering, stale-exit fencing, partial observation-only actor
projection in `SessionInfo` and telemetry, and bounded command/deadline metrics
as `dba35f98`. PR #626 then merged the immutable typed decision contract,
non-consuming `PollProducerDecision`, one-slot actor pending value, and one
move-only executor inbox/task as `9063bb1e`. PR #636 bound that transport to
production transcode startup: the actor emits the first real decision, HTTP
admission closes retry before attempt bytes escape, and one executor applies
the sole validated prepublication retry.

Merged #641 owns actor-managed transcodes after first media. That cut keeps the
existing actor deadline armed, maps terminal postpublication producer failure
to one stable proposal and `CleanupPolicy::RetainPublished`, and serves
an already-authorized playlist body plus retained init and numeric segments at
or behind the frozen frontier while requests beyond it receive typed
`producer_ended`. Fresh playlist reloads are rejected after the retained
failure proposal, so a late observation cannot expand the authorization
surface. A zero process exit is success only after ENDLIST/frontier completion
is proven. The active branch `codex/playback-control-m4-copy-lifetime` now moves
copy startup, reader classification, process-exit handling, and the sole
prepublication `Unsupported` fallback into the actor/executor. It does not
change the client wire; responses remain `action:none`.

M4 makes the same actor the sole recovery decision owner. It replaces hardware
startup grace, software startup/lifetime polling, and copy-segmenter fallback
with one exact-attempt `ProducerProgressDeadline`. One pre-publication retry is
allowed only when the actor supplies the validated recipe. After publication,
failure retains published bytes and produces one stable internal replacement
proposal; it never swaps the child in place or emits an incomplete wire action.
The existing exact-attempt supervisor remains the sole OS-child/PID owner, and
a single session executor orchestrates actor-authorized proxy changes. The full
contract is
[`PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md`](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).

Review and test state for M4:

| Gate | State |
|---|---|
| Implementation contract | **Merged in #618.** It defines deadline policy, contiguous cutoff-safe ingress with command and producer-event barriers, arm/disarm and event ordering, exhaustive action-timeout settlement, the one-retry invariant, hard rolling-process admission, process-executor ownership, publication-aware cleanup, post-publication proposal behavior, instrumentation, source ownership checks, and the race/failure matrix. |
| Static owner inventory | **Active recount clean.** `tests/playback/rolling-producer-owners.toml` exact-counts recovery/election/replacement owners and scans every Rust module under `plurxd/src`. The active cut removes the copy watchdog/election/request-verdict owners without weakening scan scope or equality assertions; the static recount reports zero mismatches across 32 source symbols and seven entrypoints, including the corrected `RetainPublished` count of 15. |
| Progress ingress | **Merged in #619.** `ProgressCoverageBatch` retains first, covered tail/deadline, first gap, latest progress, and latest telemetry with a persistent exact-attempt watermark and exit barrier. The local actor consumes that proof without flattening away publication time. |
| Passive deadline/cutoff | **Merged through #624.** The actor folds producer facts under transition plus ingress, uses fenced publication timestamps, classifies non-success exits immediately, preserves progress around bounded sequenced physical-flow barriers, fails closed when the actor/mailbox disappears, serializes actor-task exit fencing with producer transitions, and re-authorizes exact attempts before full-capacity STOP/CONT syscalls. It still emits no recovery action. |
| Operational projection | **#641 merged; copy candidate implemented.** Actor-managed transcodes retain one producer deadline after first media, freeze one stable failure proposal/frontier, reject later observations that would expand it, reap the exact failed attempt with `RetainPublished`, and return typed `producer_ended` beyond that frontier. The active cut adds typed copy-reader facts, immediate direct/takeover exit classification, and bounded completion/process rendezvous. |
| Adversarial implementation review | **Cluster correction delta approved; cumulative review pending.** Earlier rounds repaired the `Unsupported`/EPIPE ordering race, local playlist authority error, residual EOF type downgrade, response-lifecycle fixture, and exact-count/lint seams. Independent review approved `cf3bdcb9` plus its evidence mapping with no actionable P0–P3; a cumulative exact-PR-head review must still bind the complete branch and documentation commit. |
| Format/static inspection | **Complete locally.** Formatting, lint, validation catalog, history, 123 operations contracts, 52 benchmark checks, web policy, owner inventory, and diff inspection are green. The current history reports 1,125 corrective commits, 772 direct test changes, and 666 explicit mappings. |
| Unit/focused tests | **Complete locally; broad run consumed.** The one permitted broad run reported five failures; every one passes by exact name after reviewed repairs. The final client-fetch lifecycle regression passed 1/1 with 1,300 filtered tests, and the cluster CAS regression passed 1/1. Do not repeat the broad suite. |
| Full/cluster/hosted gates | **Local complete; hosted rerun pending.** One unrestricted `make cluster-check` passed. The hosted cluster failure was isolated to post-CAS freshness classification; `cf3bdcb9` is reviewed and the complete `make cluster-harness-check` passes locally. Push the synchronized head, require every hosted check green, then merge. |

## Watchdog-removal ledger

The target is not zero timers. The target is no overlapping recovery owners.
The detailed symbol-by-symbol contract is in the plan's
[deadline inventory](PLAYBACK-CONTROL-PROTOCOL-PLAN.md#7-deadlines-and-watchdog-inventory).

Merged `main` has actor-owned prepublication transcode recovery. The active
branch now also owns actor-managed transcode published-lifetime failure and
has removed that scope's compatibility progress watcher. The active
candidate cut extends that ownership to copy startup, progress, reader
classification, process exit, and the sole `Unsupported` retry without leaving
a compatibility watchdog or request handler as a second decision maker.

### Merged baseline and active-cut watchdog disposition

| Mechanism | Merged baseline and active-cut disposition | Why it remains, if retained |
|---|---|---|
| `FIRST_SEGMENT_GRACE` and the detached startup downgrade loop | **Removed from production.** `FIRST_SEGMENT_GRACE` has zero source owners, and `downgrade_one_step` is no longer a production recovery path. | The actor's one exact `ProducerProgressDeadline` and immutable retry decision replace them. |
| `SOFTWARE_GRACE` | **Present on merged `main`; removed from copy policy in the active working tree.** | Copy startup now uses the actor's exact-attempt deadline rather than a detached grace owner. |
| `PROGRESS_STALL` | **Actor policy input only in the active working tree.** | A producer cannot self-report a silent wedge, so the duration remains as the actor's progress budget without a polling recovery task. |
| `WATCHDOG_POLL`, `watch_for_stall*`, and `watchdog_active` | **Present for copy on merged `main`; removed from copy decision ownership in the active working tree.** | Typed reader facts and exact process-exit events now enter the single actor deadline/classifier. |
| `child_transition` and `replacing_child` | **Still retained as general lifecycle/resource serialization; no longer copy policy voters in the active working tree.** | Exact process install, teardown, response/path fencing, retirement, and cleanup still need serialization. They cannot select copy fallback or infer failure. |
| copy fallback/recovery | **Actor-owned in the active working tree.** | Only a typed `Unsupported` reader fact may consume the one immutable frozen direct-HLS recipe. Invalid configuration, reader failure, deadline, or process failure is final for that generation. Direct and takeover copy attempts use immediate process classification and have no retry. |
| `PREPUBLICATION_REAP_RETRY` (5 s) and `PREPUBLICATION_REAP_ATTEMPT_TIMEOUT` (2 s) | **Retained lifecycle cleanup bounds, not playback watchdogs.** | They retry bounded termination until the exact predecessor is confirmed reaped while retaining process admission and scratch; they cannot choose retry, replacement, playback failure, or response publication. |
| rolling retirement/scratch settlement | **New bounded lifecycle owners.** | Exact retirement survives caller cancellation and holds process resources through reap. Scratch cleanup makes three 5-second attempts separated by 5 seconds, then leaves the orphan for startup/maintenance cleanup; it makes no recovery decision. |
| exact-EOF settlement task | **Retained bounded response completion owner, not a watchdog.** | At most 256 visible streams can reserve one owner; each owner gets one fresh five-second deadline at exact advertised EOF, commits only complete delivery, and releases capacity on every terminal path. |
| first-media settlement | **Retained bounded handoff owner, not a watchdog.** | At most 256 settlements bridge actor authorization to published-lifetime ownership without clearing confirmed-reap state early. |
| absolute playlist preparation budget | **Retained network lifecycle limit, not a watchdog.** | One deadline covers registry/storage reads, actor observation, catalog projection, and every poll across reclassification; timeout returns retryable `503` and cannot select or replace a producer. |
| bounded HTTP actor/handoff waits | **Retained network lifecycle limits, not recovery watchdogs.** | One five-second absolute publication deadline covers admission, first-media application, buffered commit, and actor command/reply waits; cancellation is fail-closed before later actor mutation. |
| admitted local and relay body lifecycle | **New post-admission network/storage owner.** | Preparation uses the inherited request deadline; after admission, every prepared, local-file, and relay body has a 300-second absolute lifetime, while active file/network pumps also enforce 30 seconds without upstream or downstream progress. Independent bounded pumps own file/network readers and EOF authority under socket backpressure: local bodies retain one unacknowledged chunk, relay bodies two, and queued data is rejected after terminal/absolute timeout. The common finite resource lifetime makes the terminal-projection safety bound meaningful; M8 may change transport, but body-liveness bounds remain. |
| make-before-break successor publication | **New durable handoff fence, not a playback watchdog.** | A provisional successor must exist before the Store pointer CAS can end its predecessor. Activation persists an indefinitely blocked sentinel; the first exact post-commit observation arms a 372-second boundary, and publication requires an explicit Store transition to `0`. Exact predecessor acknowledgement may clear early because it prevents future old-capability admission; already-admitted bodies may drain under the predecessor's distinct never-reused URL. Without acknowledgement, only an exact elapsed-boundary proof clears the fence. M6 generalizes this transaction to quality/HDR/track changes. |
| public release and internal abort settlement | **New bounded terminal owners.** | Public DELETE elects one exact shared settlement before immediately detaching ownership; duplicates join without another task or permit. Five seconds bounds capacity admission and each caller's wait, never an admitted Store mutation. A commit-unknown End remains fail-closed in a 128-entry, 32-shard registry and retries at fanout four with 1–30-second backoff under panic supervision. The first Store `terminal_reason` wins. Ended rows persist sentinel → finite boundary → `0`, so exact owner acknowledgement and fallback completion survive ingress/owner restart without extending the window. Replay-visible terminal/absence proof plus durable projection completion settles `204`; an unreachable peer may settle only after the 372-second terminal-projection boundary covering the 62-second pre-header ceiling, 300-second body lifetime, and margin. Remote delivery is capped at three attempts with 100 ms delay, while physical reclamation remains independently bounded. M8 prepared handoff may replace the cleanup transport. |
| VOD missing-init regeneration | **New bounded lifecycle fallback.** | Four node-wide slots, a 15-second deadline, an 8 MiB ceiling, and per-key ownership through confirmed child reap prevent unbounded forks/buffers and same-key overlap. Later VOD/index hardening may delete on-demand regeneration. |
| VOD terminal cleanup/replay | **New compact lifecycle owner.** | Heavy media/process graphs release immediately after exact cleanup. A compact owner replays `410` for 60 seconds, then a 3-second, 16-way fail-closed durable confirmation permits eviction. M8 may centralize the durable replay record. |
| VOD idle/dormant retention | **Retained cache lifecycle policy.** | Sessions idle after 300 seconds; renditions become dormant after 1,800 seconds. Detached purge owns the key, child, accounting, directory, and identity cleanup before any await, preventing rebuild overlap. |
| cluster lease/takeover timers | **Retained failure fencing, not playback-progress watchdogs.** | Three-second renewal and 12-second lease bounds distinguish a live owner from a dead node. M8 replaces compatibility takeover with prepared handoff; lease expiry remains. |
| 15-second repair/flow-control tick | **Retained maintenance schedule.** | It refreshes indexes, retention, speed, pacing, and lease projection. M4 removes recovery authority from the tick; periodic maintenance remains. |

### Merged-baseline mechanisms and active-cut disposition

| Mechanism | What it currently does | Removal owner |
|---|---|---|
| copy startup/progress watcher and request-side exit inference | Present on merged `main`; removed in the active working tree | Typed copy-reader facts plus exact process exits now feed the actor, which owns the only deadline and verdict |
| copy fallback | Present as in-place compatibility policy on merged `main`; actor-owned in the active working tree | Only `Unsupported` can request one frozen direct-HLS retry; all other copy failures are final |
| `child_transition` and `replacing_child` | Retained for exact lifecycle/resource serialization, but not as copy recovery authorities | Keep until their remaining response, flow, retirement, retention, and cleanup duties receive narrower owners |
| playlist and live-segment wait budgets | Bound an HTTP request waiting for publication | M3c1 records publication in actor state; later M3 routes waiters through it; bounded HTTP deadlines remain by design |
| 15-second repair/flow-control tick | Refreshes indexes, prunes retention, records speed, evaluates flow, and claims lease expiry | Scheduling remains; M3c1 copies publication/fetch facts into the actor but recovery decisions still move in M4 |
| `SegmentIndex`, `playlist_published`, `high_segment`, `fetched_end_ms` | Catalogs rolling files and feeds pruning, byte accounting, pacing, and status | M3c1 makes actor delivery state authoritative for status while retaining these as action-path compatibility projections until M4 |
| fetched-frontier ahead-window inference | Suspends/resumes production based on download behavior | M3b replaced the time policy only after a session enters explicit mode; legacy pacing and byte/disk safety remain |
| VOD segment materialization deadline | Bounds demand for an immutable segment that is not ready | Remains permanently as one of the three approved progress deadlines |

### Client recovery owners still present

| Client | Remaining independent recovery paths | Removal owner |
|---|---|---|
| Web | persistent-wait/startup/seek timers, hls.js fatal handlers, decode rescue, Auto rung replacement, truncated-end reopen, `/status` polling | M5 makes the controller the only action owner; M6 moves Auto to prepared actions; M9 deletes old polling |
| Apple | status polling, starvation/stall/black-frame monitors, reopen budgets/queue, early-end and item-failure handlers | Apple M2 reporter, then M5 controller cutover |
| Android | status polling, buffering stall tracker, stall guards/budgets, direct reopen coordinator, error/end compatibility handlers | Android M2 reporter, then M5 controller cutover |

After cutover, only these playback progress deadlines may remain:

1. server `ProducerProgressDeadline` for a producer that cannot report its own
   wedge or death;
2. client `PlaybackProgressDeadline` because only the playback framework knows
   whether decoded media is rendering; and
3. VOD `SegmentMaterializationDeadline` because an HTTP request cannot wait
   forever for a demanded immutable object.

Lease expiry, cluster owner expiry, preparation expiry, retirement grace,
HTTP/relay limits, admission waits, and control rate windows also remain, but
they are named lifecycle/network bounds rather than competing playback
recovery watchdogs.

The 60-second terminal-ack retention window introduced by M3c3 is likewise an
idempotency bound: it retains one immutable accepted reply and triggers no
playback, replacement, restart, or failure decision. It is not a watchdog.

Cluster takeover adds one 24-second, one-shot ownership lease. The survivor
must complete an exact bootstrap renewal from the provisional worker before
durable-ID adoption and seed/publication; ordinary renewals then return to the
12-second cluster owner lease. This fixed lease and its four-second Store
deadline can fence serving authority, but they never infer a playback stall,
select quality, or restart a producer. They are cluster split-brain bounds,
not playback watchdogs.

## Remaining delivery order

1. ~~Push the synchronized PR #642 head and obtain exact-head adversarial
   approval.~~ Merged as `062530d2`; #663 closed handoff item 5 behind it.
2. ~~Monitor every required hosted job … and merge only when the reviewed head
   is wholly green.~~ Done with that merge.
3. ~~Deploy the backward-compatible server cut, then ship passive reporters
   that accept only `action:none`.~~ Deployed, and superseded: all three
   clients now consume actions, not only `action:none`. See the item 6 row.
4. Complete M5/M5.5/M6 so quality, codec, dynamic range, tracks, subtitles, and
   node placement use the same prepare/commit/abort transaction.
5. Complete semantic indexing. **Four of five merged** as
   [#700](https://github.com/pjunod/plurx/pull/700): exact intro/credits
   destinations with provenance/confidence, manual overrides, force-analysis
   controls, and queue/current-work visibility with its instrumentation. **Subtitle windows
   are not built**, and marker-destination prewarm has its counter but no
   consumer — see §"What item 8 did not deliver". Detection itself, deciding
   where an intro *is*, is a further deliberate exclusion gated on a labelled
   corpus and a false-positive-weighted evaluation.
6. Complete clustered planned/hard handoff, mixed-fleet cutover, compatibility
   deletion, playback-lab fault injection, and physical web/Apple/Android
   acceptance.

## Definition of finished

This project is finished only when M9 acceptance passes: repository checks
find only the three approved progress deadlines and named lifecycle timers;
no recovery path outside the actor/controller can replace media; transparent
prepared handoffs cover the supported recipe axes and cluster ownership;
semantic analysis publishes exact skip destinations; and the automated plus
physical playback matrices are green. Merged foundations are not a substitute
for that exit condition.
