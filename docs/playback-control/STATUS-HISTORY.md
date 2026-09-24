# Playback control — status history

**Status:** done · records moved verbatim from `STATUS.md` on 2026-09-24

**Moved here from [STATUS.md](../../STATUS.md) on 2026-09-24**, verbatim, by
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
M6. Each section keeps its original heading under the date it was first
recorded in `STATUS.md`; relative links are re-based to this folder and
nothing else changed. Newest first. These are records: a section here
describes the state on the day it was written, and
`tests/operations/test_status_pr_claims.py` keeps holding it to the same
merged-pull-request rule it held in `STATUS.md`.

## 2026-09-13 · Apple's notice strip was a dead end, and two rows of the readiness card were false

**[#297](http://forge.lan:3000/noirr/plurx/pulls/297), titled `WIP:`.**
Paul's to merge, and this branch has not been. (Worded carefully for the
same reason #291 was: `test_status_pr_claims` reads that number as landed,
because this repository's history carries a GitHub-era `(#297)` from before
the numbering restarted on Gitea and the check cannot tell two numbering
spaces apart. Twice now — the collision is a property of the check, not of
either page.)
A reachability audit of `main` found one real defect left on Apple after the
three surface-reach branches merged, and it was the kind this contract exists
to stop: a surface that tells the viewer something and gives them nothing to
do about it.

**The banner rendered no actions at all.** §3.1's `banner` is "a notice strip
with **the fault's actions**; the picture is untouched", and `failureView` was
the only view in the client that drew `surface.actions` — gated on a blocking
surface. So `refused`, the one class whose whole point is that the predecessor
keeps playing while the viewer's change did not, drew a sentence and no Try
again. Physical recipe (b) forbids exactly that outcome. §3.2's demoted
"Playback recovered" banner lost its actions the same way, which defeats the
reason the demotion keeps them: so the viewer gets the specific reopen when the
buffer drains.

The strip has its own action row now, through the same button and the same
labels as the full screen, so "Try Again" cannot come to mean two things. It
does **not** add the guaranteed Close the full screen adds — behind a banner is
the viewer's film, and a Close on a notice is a button that ends playback that
is fine. A Close the fault itself carries is still drawn: actions are a
property of the fault, and the presenter does not second-guess the owner about
them. That distinction is what the two banner tests hold from either side.

Two smaller faults in the same code went with it. A **demoted** banner read
`detail ?? title` and so announced the thing that had failed at the exact
moment the picture came back — §3.2 rewrites the title to "Playback recovered"
and deliberately keeps the failure sentence in `detail` for the fault's own Try
again. And the strip asked whether the viewer was *blocked* rather than what
*kind* of surface this is, so every `indicator` drew the in-chrome capsule and
the strip at once: one fault, two overlays.

### The readiness card said two false things

`playbackSurfaceReadinessCard` is deliberately static prose — the browser
cannot read a build number or a repository file, so a computed-looking pill
there would be a computed pill that lies. The cost is that nothing sweeps it:
`validation/doc_versions.py` checks the two client READMEs,
`APPLE-CLIENT-PARITY.md` and `docs/STATUS.html`, and has never read
`index.html`. Both client rows went stale the moment a client shipped again —
the Apple row claimed build 150 and a 532/518 run two builds after both had
moved, and the Android row claimed build 92 and 612 tests against a README that
says 93. Both now read what is in the tree, and the comment above the function
says why they drifted and what the next reader has to do. They stay advisory:
no control, no gate.

### What was run

`make apple-build` (both schemes) and `make apple-test` on iPhone 17 Pro
(iOS 26.5) and Apple TV 4K 3rd generation (tvOS 26.5): **552 iOS tests and 538
tvOS tests executed, zero failures**, with
`testPlaybackSurfaceModelRunsEveryContractCase` confirmed to have run under
each and all three new tests confirmed passed under each. Four mutations, each
failing a named test against a run that executed 552.

On the VM: both fences PASS with no new `MIGRATION_BUDGET` entries, all five
node playback tests (62 surface cases), `make web-check` exit 0,
`tests/operations` 356 OK, `scripts/validate lint` OK.

### What this does not close

The tvOS focus question. The banner's buttons are focusable while the picture
plays, so a directional press with the chrome hidden can land on Try Again
rather than revealing the controls. That is arguably the better answer — the
action is right there — but it is a device behaviour and no simulator settles
it. It joins §6's eight runs, none of which has happened.

## 2026-09-13 · Every class in the playback surface contract can now be drawn on Apple

**[#291](http://forge.lan:3000/noirr/plurx/pulls/291), titled `WIP:`.**
Paul's to merge, and this branch has not been. (The wording is careful
because `test_status_pr_claims` reads that number as landed: this
repository's history carries a GitHub-era `Merge pull request #291` from
before the numbering restarted on Gitea, and the check cannot tell two
numbering spaces apart. Recorded rather than worked around in the check.)
Build 147 landed the Apple presenter and an audit found parts of it
unreachable. Four fixture source rows had no Apple raise site, one view branch
could not be reached at all, and the one mutation the last round could not kill
is now killed.

### The one that mattered

**`buffering` could never be drawn on this client.**
`waitingToPlayAtSpecifiedRate` still drove the legacy `isPlaybackWaiting`
spinner, painted by a second `if` in `PlayerView` underneath the one
`isChangingStream` painted — two owners for one pixel, which is the defect the
contract exists to kill. A wait is now a `media_waiting` fault (row 12) drawn
as `buffering` once it has lasted the contract's 350 ms, and both legacy flags
are gone. The debounce is the reducer's; nothing on the client keeps a second
one.

Row 12 is declared for the `attached` context alone, which is what "after
start" means, so a wait before the first frame raises `client_preparing`
(row 10) — the staged start it actually is. The legacy overlay drew in both
cases and still does, with the words it always composed.

### The other three rows, and the dead branch

- **`client_preparing`** — the staged loading overlay was `isChangingStream`
  and raised nothing. It raises on the transition that sets the flag and
  settles on the transition that clears it, carrying the viewer request it
  belongs to, so the fault's life is exactly the flag's life through an attach,
  a refusal and a supersede alike.
- **`readiness_deadline_rungs_left`** — contract row 14 names Apple's
  `retryAfterReadinessTimeout` outright, and that path borrowed the ladder's
  `owner_recovery_step`. It raises row 14 now. Same class, same pixel; the
  ledger stops calling a readiness timeout an ordinary fallback.
- **`log_only`** — raised by nobody on any client, so `surface_log_only` had
  never been emitted. Apple's three row-18 equivalents raise it: a prepared
  successor given up by any route that is not a committed switch, a control
  exchange that came back with nothing (where `404 session_gone` on a
  successor's first exchange arrives), and a status poll that answered
  nothing. One row per fact per attached generation. `postClientLog` is not a
  site, because this raise posts a client log.
- **The Keep Waiting button** — `PlayerView:1529` strips `keep_waiting`
  whenever `retry` is present, deliberately and documented, so the label at
  `1560` was unreachable. The behaviour stays, the label is deleted, and the
  reason the web is adding a real one is written where a reader will ask.

### And one the audit did not name

Closing the first gap surfaced it: a `buffering` fault was retired only by
presentation evidence, and a paused picture never produces another sample, so a
buffer that filled while the viewer had paused left a spinner over a still
frame until the generation changed. The overlay outliving the thing it
described — the defect this contract exists to kill, reintroduced by the
migration. The Android session found it independently and the web had it too.

**Ruled: a `buffering` fault is about a player that WANTS media, so a viewer
who pauses makes it about nothing.** The fixture and reducer half arrived from
`web/playback-surface-reach` as a cherry-pickable commit (`1e19b133`), together
with `segment_503_not_yet`'s codes (`f7cad0fd`); both were cherry-picked onto
this branch rather than rebased, and the Swift half is ported here. The Apple
wiring is `wantsPlayback` — the viewer's own transport intent, and the flag
`stopForBlockingSurface()` deliberately leaves alone, so the owner's own stop
is filtered out by construction rather than by a rule somebody has to remember.
`playback_not_requested` retires `buffering` and no other class: a `preparing`
start has not been paused by a viewer who has not seen it, and a prompt is
answered by the viewer rather than by a transport change.

### Two findings from the review, both in code this branch wrote

**An indicator was being drawn as the blocking box.** The new
`showsProgressSurface` answered true for `kind == .indicator` and the view fed
that into its one render — the centred, full-frame box. An indicator is by
definition a progress fault *while the picture is presenting*, and `recovering`
retires only on presentation that postdates its raise, so a compatibility rung,
a node failover or a readiness deadline put a modal spinner over a picture that
was playing perfectly well. The commit removing overlays-over-moving-pictures
introduced one. §3.1's two renders are two views now, chosen by a pure function
so the distinction is provable rather than read off a SwiftUI body.

**"By construction" was not true.** `stopForBlockingSurface()` does leave
`wantsPlayback` alone, but five other terminal stops wrote `player.pause()`,
`isPlaying` and `wantsPlayback` out by hand immediately before raising, so all
five fired `playback_requested(false)`. Benign — each raises a class that
outranks `buffering` — but the guarantee did not exist and nothing tested it.
All five go through the one helper now, which takes `revokingPlaybackIntent:`
and says what it is doing; §3.4's obligation has one implementation and the
viewer's transport intent has one owner-side writer.

### One thing added that is not a raise site

The presenter now has a clock. Every timing the contract states is measured by
the reducer when its caller applies an event, and AVPlayer's periodic observer
stops firing the moment the film clock stops — which is exactly when a wait
needs drawing. A 500 ms task feeds `.tick`, the twin of the web's
`playbackProgressTick` on the same interval. It samples nothing, detects
nothing, and moves no threshold, budget, detector or ladder.

### A6, closed

A6 survived because no test had ever had two create sequences alive at once.
The overlap is ordinary: a viewer leaves a cold start still waiting on its
create and puts another title on. The abandoned sequence's `defer` must retire
only its own epoch — retire the counter unconditionally and the replacement is
left unwatched, so its sixty-second deadline fires into a guard that no longer
matches and a title that never starts sits on a spinner for ever with no
prompt.
`testAnAbandonedCreateSequenceRetiresOnlyItsOwnEpoch` drives that, taking the
abandoned sequence's release of its own late session as proof its `defer` ran.

### What was run

`make apple-build` (both schemes, `** BUILD SUCCEEDED **` twice) and
`make apple-test` on iPhone 17 Pro (iOS 26.5) and Apple TV 4K 3rd generation
(tvOS 26.5) on the macOS runner, on the rebased tree: **549 iOS tests and 535
tvOS tests executed, zero failures**, with
`testPlaybackSurfaceModelRunsEveryContractCase` confirmed to have run under
each. Baseline before this branch was 532 and 518.

Fifteen mutations were applied on the runner one at a time and reverted; each
failed the test named for it against a run that executed 543, 546 or 549 tests.
The first attempt at the first mutation executed **zero** tests and reported no
failures — the wedged-simulator trap, a false survivor if believed — and was
re-run after `simctl shutdown all` and `erase`.

On Linux: both fences PASS with no new `MIGRATION_BUDGET` entries,
`tests/playback/playback-surface-contract.test.js` (60 cases),
`tests/playback/web-policy.test.js`, `make web-check` exit 0,
`tests/operations` 355 OK, `scripts/validate lint` OK.

### What this does not close

- **Row 14's proof is a source-shape pin**, which is exactly the kind of proof
  the last round found worthless for A3. `retryAfterReadinessTimeout` is
  reachable only from an `open()` whose `seekWhenReady` times out over a
  decoding player, which no headless XCTest has. The pin checks the row against
  the shared table rather than against a spelling, so a wrong or invented row
  fails — but it would not catch a semantically identical rewrite. Known gap,
  not an implied guarantee; the honest route is §6's simulator recipe.
- **The §6 simulator and device recipes still have not run.** Every one needs
  a live `plurxd` with real media, and none was reachable from the runner.

## 2026-09-13 · The Android playback surface has no unreachable sources left

**Landed as [#289](http://forge.lan:3000/noirr/plurx/pulls/289).** An audit of
the merged playback-surface work found four of
the contract's sources with no Android raise site at all, so four rows of §3.3
described behaviour the client could not produce. All four are closed, one is
recorded as a deliberate parity gap, and one dead branch is gone.

### `media_waiting` — the one that mattered

`STATE_BUFFERING` fed `Controller.isPlaybackWaiting`, and `PlayerScreen` drew a
spinner from it *beside* the presenter's own progress surface. Two things
decided what covered the picture, which is the exact defect this contract
exists to kill — and because the legacy path owned the wait, the `buffering`
class could never be drawn on Android at all.

The wait now reaches the surface as every other reason does. `sampleSurfaceWait`
raises `media_waiting` from the loop the stall watchdog already runs, on the
same predicate (`playbackIsWaiting`) the spinner read, in the `attached` context
(`establishedPlayback`). It is raised **once per wait** — the sampler runs every
second and a fault per sample would make the ledger's history the sampler's
cadence — and the class's 350 ms debounce is the reducer's, not a second timer.
`isPlaybackWaiting` is deleted. Nothing about *when* a wait is detected moved;
what moved is who owns the pixel.

### `client_preparing`

Retiring the legacy spinner leaves the cold start with nothing to draw, because
before the first frame there is no wait in the `attached` context — there is an
open. `restartAt` is the one function every open goes through, so it raises
`client_preparing` there, right after the new generation attaches and only when
playback is actually requested. It carries no sentence: the screen's existing
wait copy is what this window said before, and a reopen still says why, because
a recovery step raised after its `restartAt` is newer and wins the progress tie.

### `log_only`

Raised by nobody on any client, so `surface_log_only` had never been emitted.
Row 18 is "the incumbent is untouched and the event is the only trace", and
Android has three of those: a prepared successor abandoned *after failing*
(a deliberate abandonment is not a failure and says nothing), the playback
control reporter giving up — which is where a successor's 404 `session_gone`
lands, and which needed a `onGaveUp` hook the reporter did not have — and
session-status polling that stopped answering, once per polling job rather than
once every two seconds. `SurfaceLog` gained a `detail` field, because an event
that is the only trace has to say what happened.

### `repeated_early_end` — recorded as a parity gap, not implemented

The row says "unchanged", and the two clients that have it disagree: the web
tolerates a second at the same position and gives up after four tries, Apple
tolerates 250 ms and gives up on the second. There is no single semantics to
port, and picking one is inventing a threshold. Worse, **Android has no
early-end recovery at all** — `STATE_ENDED` posts progress and autoplays — so
"repeated" has nothing to count, and building the ladder it would count is a new
detector plus a new recovery path, which implementation §6 forbids here. The
argument is in `docs/clients/ANDROID-CLIENT-PARITY.md` under "Repeated early
end".

### The fixture's `codes`, honoured

A playlist or segment 503 was unconditionally `segment_503_not_yet`. Contract
§3.3 row 8 is a 503 *with a "not yet" code*, and Android is the one client that
can read the body (`InvalidResponseCodeException.responseBody`).

The adapter asks the ROW rather than deciding: `surfaceRowAdmitsCode` reads the
row's own list off the transcribed table. A row that lists no codes is claiming
its status outright — demanding a code from one would make the row unreachable,
which is the defect this change removes, not one to add. A row that *does* list
codes claims only those, so a 503 whose code the row does not name falls through
to whatever row the code actually names (`surfaceSourceForCode`), and otherwise
to nothing. 401/403 and 410 stay status-based per §3.5, and the server's
sentence and position survive either way.

**What that changes is the attribution, not the class.** On the recovery path the
owner is recovering, because `PlaybackPolicy.playbackErrorAction` said so — and
that is a ladder decision this work may not move (§3.5 puts the adapter *after*
it, on the outcome). So an unadmitted 503 still surfaces as `recovering`; what it
no longer does is claim to be `segment_503_not_yet` while doing it. It is
reported as `owner_recovery_step`, which is what it actually is, and the ledger
stops attributing the owner's own reconnect to a server refusal that never said
"not yet". Making such a 503 *stop* the player instead would be a ladder change,
and it is not in this PR.

### `SurfaceAction.ForceTranscode`

Unreachable, and correctly so: it is a web affordance. The duplicated filter in
the two renders is now one `surfaceActions`, which is the single fact that makes
it unreachable. **The `when` arm itself could not be deleted** — Kotlin 2.3
requires a `when` statement over an enum to be exhaustive — so it names its own
guarantee instead of silently swallowing.

### The pause ruling, ported

The first round of this branch flagged a residue rather than fixing it: a
`buffering` fault raised while playing and then *paused* never retired, because
§3.1 retired `buffering` on presentation evidence and `attached_retired` only,
and a paused picture produces no more samples. The Apple session found the same
hole independently and the web had it too — the overlay outliving the thing it
described, reintroduced by the migration itself. It is now **ruled**, and this
branch carries the two cherry-picked fixture commits plus the Kotlin port:

- `playback_not_requested` joins `SurfaceRetirement` and
  `SurfaceClass.Buffering.retiredBy` — and **no other class's**. A `preparing`
  start has not been paused by a viewer who has not seen it yet, and a blocking
  prompt is answered by the viewer rather than by a transport change.
  `PlaybackSurfaceReducerTest` asserts that list is exactly `[Buffering]`, twice:
  once against the transcribed table and once against the fixture read directly.
- `SurfaceEvent.PlaybackRequested` joins the reducer. `false` retires every
  fault whose class names the reason; `true` moves nothing, because the raise
  sites decide what comes back.
- **The filter this client needs.** Media3 reports every app write of
  `playWhenReady` as `PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST` whoever wrote
  it, so the owner's own stop-before-raise is indistinguishable from a viewer's
  pause at the callback — and a stop that retired the fault it is about to raise
  over would be the owner deciding what the presenter shows. `Controller` keeps
  the distinction as a **level**: the `SurfaceOwnerPlayer` setter marks it
  *before* writing, because Media3 may deliver the callback before the setter
  returns, and the next request for playback clears it. A level rather than an
  edge, so a stop Media3 never reported — the player was already paused — cannot
  swallow a later genuine pause; nothing can pause an already-paused player, and
  the only event that can follow is a resume. No new detector, no new timer.

### The fixture's codes, now that they exist

The sixteen `segment_503_not_yet` codes are transcribed into `SURFACE_SOURCES`
off the fixture. `surfaceRowAdmitsCode` already asked the row rather than
deciding, so nothing else in the adapter moved — the row simply stopped claiming
every 503. A `vod_disabled` on a segment is no longer *named*
`segment_503_not_yet`; it is `owner_recovery_step`, and its class is still
`recovering` because the owner is in fact recovering. See the attribution note
above: changing that would be a ladder change.

### The blocker review found, and the regression it was

Retiring `isPlaybackWaiting` left a hole the first round did not see.
`sampleSurfaceWait` answered only past the first frame, and `client_preparing`
was raised from exactly one place — `restartAt`. But `executeSeek` **bypasses**
`restartAt` on purpose (its own comment says so), and all four of its transports
call `beginPlaybackAttempt`, which clears `establishedPlayback`;
`retryMediaOnNextNode` does the same. In those windows the player is
`STATE_BUFFERING` with `playWhenReady` true and `attachSurfaceGeneration` has
just retired every fault about the outgoing generation — so the surface was
`None` and the screen drew nothing. **Every seek and every node failover was a
frozen or black picture with no spinner and no text until the first frame
rendered**, which is exactly the window the legacy spinner used to cover and the
one regression its deletion had to avoid.

The sampler now answers the wait by context rather than only past the first
frame, which is the split Apple's presenter already makes: `media_waiting` with a
picture established, `client_preparing` in the `start` context before one,
because before the first frame the same wait is not a wait — it is the open.
`mediaWaitSource` is that decision as a function rather than two branches at a
call site no JVM test can reach, so the tests can name the entry path each one
stands for.

### Evidence

`clients/android`, in the pinned image on lab6 (`make android-test` / `make
android`): `:app:compileDebugKotlin`, `:app:testDebugUnitTest`,
`:app:lintDebug`, `:app:assembleDebug`. Counts read from
`app/build/test-results/testDebugUnitTest/*.xml`, not from the log —
**634 tests, 0 failures, 0 errors, 0 skipped** across 88 files. `main` was 612
when this branch opened and has changed no Android source since (#290 is web,
fixture and docs), so the 22 new tests are this branch's. Both fences, all five
node playback tests (62 surface cases), `make web-check` exit 0,
`tests/operations` 356 OK and `scripts/validate lint` on the VM. **Eleven
mutations** applied on the build host, each failing a named test against a run
whose XML shows 634 tests executed, each reverted; the table is in the PR.

Rebased onto `origin/main` after #290 merged; both cherry-picked fixture commits
dropped out as duplicates, exactly as expected.

**Unrun:** no emulator or physical device. §4.4's recorded emulator run — inject
a 503 on a segment after 30 s of playback — has not been done, and M4's recipes
remain unclaimed.

## 2026-09-13 · The web half of the playback surface contract is reachable, and four rulings are closed

**Landed as [#290](http://forge.lan:3000/noirr/plurx/pulls/290).** An audit found parts of the
merged work unreachable — two contract rows that no web site ever raised, an
action in the vocabulary that no site ever offered, and a deadline that could
not fire. All of it is closed here, together with the four rulings that were
waiting on somebody (§*Open rulings* below says where each landed).

- **`degraded_notice` had no web raise site.** Six §3.3 row 17 notices left the
  player through `toast()` — a 2.2-second strip with no class, no identity and
  no ledger row — and picture-in-picture's failure left through a `catch` that
  dropped the browser's own sentence. They raise now, with the copy they
  shipped with.
- **`log_only` was raised by nobody, on any client**, so `surface_log_only` was
  a log event the fleet could never emit. Four web sites raise it: a prepared
  successor abandoned, one that failed on its own, a refused control exchange,
  a stats poll that could not answer. The incumbent is untouched at all four,
  which is the row.
- **`keep_waiting` never reached a button.** It does now, at all three
  `owner_exhausted` raises, and a test fails if it disappears or if pressing it
  does more than re-arm the ladder.
- **The create-retry deadline could not fire.** M5's 60 s sequence ran inside a
  pre-existing 20 s preparation bound, so the viewer got "Playback could not
  prepare." where the server had been saying "still building". The bound stays;
  the outcome is now the sequence's own exhaustion with the server's own
  sentence, and the branches that could never run are deleted rather than left
  reading like a bound.
- **`segment_503_not_yet` got its codes**, in a separate cherry-pickable commit
  the Apple and Android sessions rebase onto.
- **A `buffering` fault that the viewer pauses under now retires.** Found
  independently by the Apple and Android sessions and true of the web too: §3.1
  retired `buffering` only on presentation evidence, and a paused picture never
  produces another sample, so a buffer that filled while paused left a spinner
  over a still frame until the generation changed — the overlay outliving the
  thing it described, reintroduced by the migration itself. A `buffering` fault
  is about a player that WANTS media, so a viewer who pauses makes it about
  nothing. New `playback_requested` event, new `playback_not_requested`
  retirement reason on `buffering` **and on no other class**, wired to the web's
  own `wantsPlayback` transport edges. Its own cherry-pickable commit, second of
  the two.

Not done here, and named rather than implied: Apple and Android still raise
neither row 17 nor row 18, and no part of this has been seen in a browser — the
evidence is the lane (`make web-check`, both contract tests, both fences, the
whole of `tests/operations`, `scripts/validate lint`) plus a five-mutation table
in the pull request.

## 2026-09-13 · The playback surface contract — built, both clients compiled, unverified on hardware

**Merged to `main`: M0 (#276), M1 (#277), M2 (#280), M3 (#279) and M5 (#282).
Both clients now compile and both unit suites pass — the Kotlin on 2026-09-13
on both of the hand-off's routes (see (2) below), the Swift the same day on
both simulator destinations (see "What the Apple build and test run found"
below). M4 and M6 are not done, and no physical device has run any of it.**
The effort replaced three imperative error channels — the web's
`setLoading()`, Apple's
`failed`/`playbackError`/`playbackFailureTitle`/`playbackNotice`, Android's
`onError`/`playFailure`/`playbackNotice` — with one fixture-driven presenter
per client that renders a surface from typed faults and the player's own
presentation evidence. It closes the defect in Paul's tablet photo: "Playback
stopped (ERROR_CODE_IO_BAD_HTTP_STATUS)." over a picture that was visibly
still playing, behind a transparent overlay, with the stall watchdog free to
restart the stream underneath it.

The contract is
[PLAYBACK-SURFACE-CONTRACT.md](../clients/PLAYBACK-SURFACE-CONTRACT.md) (v2,
ruled, reviewed and answered); the build plan, the amendments and the recipes
are
[PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md](../clients/PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md).

### What each milestone did

- **M0 (#276) — the fixture, the reference reducer, the fence.** One JSON
  fixture of ordered-event cases that every client's presenter must run, the
  generated class/source/timing blocks the three docs embed verbatim, the
  SURFACE section of the Playback debug field list, and
  `scripts/playback-surface-fence` with a per-file migration budget for the
  110 pre-contract write sites so no new one could be added while the
  migration was in flight.
- **M1 (#277) — the web.** `renderPlaybackSurface` is the only code in
  `index.html` that writes the overlay, its two new non-blocking surfaces, or
  the `failed` class; every former call site raises a typed fault instead.
  §3.4's stop-before-raise landed at eleven sites. Classes that were
  full-screen are an indicator or a banner now — a failed change, an explained
  "not yet", an automatic downshift, a control hold — which is the point of
  the migration and a visible change.
- **M2 (#280) — Apple.** `PlaybackSurfaceModel.swift` is a pure presenter: no
  AVFoundation, no timer, no side effect but the log entries it returns. The
  four published failure fields are deleted. `fail()` decides on whether a
  picture is *presenting* rather than on whether an item is *attached*, so a
  readiness verdict over a black screen stops the player and prompts while a
  create failure over a playing predecessor is a banner. A 401 or 403 anywhere
  stops the player and offers Sign in (ruling R1).
- **M3 (#279) — Android.** `PlaybackSurface.kt` is the same reducer in Kotlin,
  carrying the fixture's tables verbatim so a client that skipped a port fails
  rather than drifts. Every blocking site is a **named** method on
  `PlaybackSurfaceOwner` that does its **own** stop, deliberately duplicated so
  deleting one stop fails exactly one test and the failure names the site. Two
  rows of implementation §3.4 were wrong about the code and were amended in the
  same commit as the fix.
- **M5 (#282) — the three bounded recovery additions.** The only PR of the
  effort that changes what the player *does*, which is why it was its own PR by
  ruling. A create the server refuses with a "not yet" code is re-posted under
  the **same** `request_id` after 1 s, 2 s and 4 s, bounded by an **absolute**
  60 s watchdog, `start` context only, with a late success **released** rather
  than attached. The web gets one bounded `hls.startLoad` per attach, sharing
  its budget with the `segment_503_not_yet` row. Android recovers
  `BEHIND_LIVE_WINDOW` on **finite** timelines by seeking back to the last real
  position, once per attach — Media3's `seekToDefaultPosition()` is a live-edge
  policy and is deliberately not used.

### What the fence holds at zero

`scripts/playback-surface-fence` has **no `MIGRATION_BUDGET` entry above
zero left**. `index.html`, `PlayerController.swift`, `PlayerView.swift`,
`PlayerSurface.swift`, `playback-policy.js`, `Controller.kt`,
`PlayerScreen.kt` and `PlaybackSurfaceOwner.kt` are all at zero: every surface
write in the shipped players is inside a presenter's render or a publish
region with a required anchor. M5 added three rules the migration itself
needed — the element **lookup** is guarded rather than the property spelling,
because a computed member write names no property at all; the owner file is
scanned with a rule that does not require a receiver, because inside that file
there is none; and a presenter is scanned by an **inverted** rule (no player
call, no timer of its own, no control-plane call), which is contract v2's
actual thesis and was exempted wholesale before.

The fixture is **60** ordered-event cases. The web presenter runs all 60 in
`tests/playback/playback-surface-contract.test.js`; the Apple presenter runs
all 60 in `testPlaybackSurfaceModelRunsEveryContractCase`, confirmed passing
on the iOS **and** the tvOS destination; the Android presenter runs all 60 in
`PlaybackSurfaceReducerTest.everyFixtureCaseRuns`. All three were executed for
the first time on 2026-09-13.

### What the Apple build and test run found

On a Mac (macOS 26.6.2, Xcode 26.6, xcodegen 2.46.0), against this branch:
`make apple-build` compiled **both** schemes clean, and `make apple-test` ran
the whole suite on **iPhone 17 Pro (iOS 26.5)** — 532 tests — and on **Apple
TV 4K (3rd generation) (tvOS 26.5)** — 518 tests — with **0 failures on
each**. Every test the hand-off prompt names by name ran and passed under both
destinations.

Four defects in code written blind had to be fixed to get there: one compile
error (a test still read `PlayerController.failed`, the flag M2 deleted) and
three test failures (a source-shape assertion looking for the literal
`player.pause()` that M2 had moved into `stopForBlockingSurface()`; M5's
change-context test, which asserted a second `/decision` request that a warm —
prepared — quality change never makes, and whose precondition no headless test
could reach; and a blocking raise set up over a still-`presenting` picture).

**That third one was not a test bug. It was the code reporting a shipped
defect, and adjusting the test is what hid it.** `stopForBlockingSurface()`
paused the player but never told the presenter the picture had stopped
presenting, and every owner site raises its blocking fault synchronously right
after. So a 401 or 403 during a quality change — the R1 case, over a picture
that was playing — raised its `stopped` terminal into a model that still
believed the picture was moving, §3.2 resolved the disagreement against it,
and the viewer got a "Playback recovered" banner with **no Sign in and no
Close**. Permanently: a demoted fault is never promoted again, by design. Not
a race — the raise always precedes the next sampler tick. The owner's stop now
records its own evidence, which is the one-line fix, and the assertion that it
still does is pinned in `testOnlyTheEvidenceSamplerEverFeedsAPresentingEvent`.
No threshold, budget, detector or ladder was retuned.

The prompt's six pinned mutations were applied and reverted one at a time.
**A1, A2, A4 and A5 were each killed by the test pinned to it**; A5 and A6 are
also killed by `tests/playback/web-policy.test.js`, as predicted. A3 —
deciding presentation on `timeControlStatus` instead of the position delta —
**is killed by `testOnlyTheEvidenceSamplerEverFeedsAPresentingEvent`**, which
forbids the token outright. But that is a source-shape pin, and the
semantically identical mutation that avoids the banned word — dropping the
position delta and keeping the rate — survived all 531 tests. So the detector
was pinned by spelling and proved by nothing. `surfaceIsPresenting` is now a
pure function of the evidence, and
`testPresentationEvidenceIsAMovingPositionAndNeverATransportStatus` kills that
survivor. **A6 is the one genuine survivor**: no Swift test, exactly as the
prompt predicted, and the field route is §6.6.

### Exactly what is not done

1. **The Apple client compiles and its suite is green — but §6 of the
   hand-off has not been run.** `AppleClientTests`' surface cases, the eight
   owner-stop killers, the R1 and B4 pins and M5's three
   `PlayerOperationOwnershipTests` cases have now all executed and passed, on
   both destinations. What has *not* run is any of the eight simulator and
   device recipes in §6 of
   [PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md](../clients/PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md):
   every one of them needs a live `plurxd` with real media — a cold NAS that
   answers 503 `startup_timeout`, a forced 401 on a quality change, a create
   that lands after the 60 s deadline — and no server was reachable from the
   build machine. The unit suite cannot see any of those behaviours, and one
   shipped defect this branch fixes — the 401 terminal demoting itself to a
   banner — is exactly the kind of thing only §6.2 would have caught.
2. **The Kotlin compiles and its JVM tests pass. Nothing has run on a
   device.** M3 and M5's Android code was written on Linux with no SDK and no
   disk for a Gradle build; it was compiled for the first time on 2026-09-13,
   on both of the hand-off's routes. The **pinned image** on `lab6`
   (10.42.4.14, x86_64, Docker 29.1.3, so `linux/amd64` is native and
   nothing is emulated): `make android-image`, `make android-test` and
   `make android` all green — this is the run CI does. The **local SDK** on
   `maca.lan`: JDK 25, Homebrew command-line tools, platform
   `android-37.0`, build-tools 36.0.0 and 37.0.0, Gradle 9.7.1, with
   `:app:compileDebugKotlin`, `:app:testDebugUnitTest`, `:app:lintDebug` and
   `:app:assembleDebug` all green. Both routes report the same
   **612 tests, 0 failures, 0 skipped**, and the local-SDK one is stable over
   five consecutive `--rerun-tasks` runs on the Mac. The APK `make android`
   produced carries `versionCode 92`, read back out of the APK itself with
   the image's own `build-tools/37.0.0/aapt2 dump badging` (`aapt2` is not on
   `PATH` in the image) and independently out of `output-metadata.json`.
   `PlaybackSurfaceReducerTest`
   (7 cases, `everyFixtureCaseRuns` over all 60 fixture cases included),
   `PlaybackSurfaceOwnerTest` (14), `CreateRetryTest` (10),
   `BehindLiveWindowRecoveryTest` (6) and `PlaybackInfoContractTest` (4) all
   ran and all passed. One compile error was fixed and it was in a test, not
   in the app: `Cannot infer type for type parameter 'T'` on the two
   `?: emptyList()` elvis arms of `PlaybackSurfaceReducerTest`. No main-source
   Kotlin was changed, so `versionCode` stays at 92. All eight of the
   hand-off's K1–K8 mutations were applied and every one killed its named
   test; K7, as the hand-off predicts, is killed by
   `tests/playback/web-policy.test.js` and by no JVM test. What is still
   missing is every part that needs hardware: no instrumented test, no
   emulator, and none of the twelve recipes in §5 of
   [PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md](../clients/PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md).
   What was done before any of this, and is evidence about the semantics and
   not about the Kotlin: the shipped reducer was transliterated into Python,
   run against every fixture case, and fuzzed 3,500 sequences differentially
   against the shipped JS reducer with zero divergences.
3. **M4 — physical verification — has not been run.** Not one of §7's four
   recipes has been executed on an Apple TV, an iPhone or an Android TV. No
   `surface_disagreement` has been observed, and that is an absence of looking
   rather than an absence of rows. The script is
   [PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md](../clients/PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md).
4. **M6 is gated on a measurement nobody has taken.** The Android remux-seek
   landing is built only if the achieved origin differs from the requested
   start by more than 250 ms **and** the first frame landed at the origin. The
   procedure is
   [PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md](../clients/PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md).
   One correction found while writing it: §4.7 says to capture the origin
   "from the client log", and there is no such log line —
   `ProgressiveMediaOrigin.acceptResponse` has no logging at all. The prompt
   gives two ways that do exist.
5. **One ruling is still open; four and the web half of a fifth are decided.**
   See
   [Open rulings — the playback surface contract](#open-rulings--the-playback-surface-contract)
   below.

What *has* run, on every one of those PRs and on this one: the node contract,
policy, control and player-DOM tests, both fences, the whole of
`tests/operations`, and `scripts/validate lint`. Developer → **Playback
surface contract** carries the same list as an advisory readiness card — it
gates nothing and never will; contract §5 says so outright.

### The two things the fence cannot do

It is a ratchet against the spellings it knows, not a proof: it knows more of
them than it did, each with a must-trip fixture line, but it cannot see a
write it was never taught. And it says nothing about a surface being *right* —
only about where it is written.

## 2026-09-13 · Open rulings — the playback surface contract

Five questions the effort could not answer for itself. **Paul ruled on all
five 2026-09-13 and delegated the open ones; four are closed, and the fifth is
closed on the web and open on the two native clients.** Where each landed is
below; the reasoning is in §4.6 of
[PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md](../clients/PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md).

**1. Keep waiting is offered on the two stall prompts, and the two rows nothing
raised now have raise sites — web done, Apple and Android open.** *(Decided;
the web half landed in `web/playback-surface-reach`.)* The two stall sites share
a new `playbackExhaustedActions` (`playbackStallActions` with the action in
front). It is deliberately not folded into `playbackStallActions` itself,
because that list is also what the D1 terminal verdict and the diagnosed-stall
`decoder_failed` carry, and those are `stopped` — a recipe the server has ended
leaves nothing to wait for. The **create-exhaustion** prompt does not offer it:
nothing is attached there, so `armStall` arms a watchdog `stallDiagnose` returns
from immediately, and the button would clear the prompt and do nothing. The
handler is unchanged and a test now fails if the button disappears from the
stall prompts, if it appears on the create one, or if pressing it does more than
clear `recoveringStall` and re-arm `armStall`.

The same branch gave `degraded_notice` its first web raise site — six of them:
the pre-play HDR/burn refusal, the same refusal from the subtitle menu, the
decode rescue, the Auto rung downshift, the supply rescue and a
picture-in-picture that would not start, all of which used to leave the player
through `toast()` or, for PiP, through a swallowed `catch` — and `log_only` its
first raise site on **any** client, so `surface_log_only` is emitted at last:
a prepared successor abandoned, one that failed on its own, a refused control
exchange (`session_gone` on a committed successor's first one included), and a
stats poll that could not answer.

**Still open:** Apple and Android raise neither row either. Their `degraded`
notices (`playbackNotice`, the PiP persistent error) and their row-18 events
(prepared-successor abandonment, telemetry and reporter failures) are the same
shape as the web's and want the same treatment; that is native work and is not
in this branch.

**2. The web's create-retry deadline: keep the 20 s, make the outcome honest.**
*(Closed in `web/playback-surface-reach`.)* The bound does not move — §6 forbids
it, and 20 s is the right bound for a client whose whole open is bounded at
20 s. What changes is what it produces. `beginPlaybackPreparation` gained one
hook, `expiry`, where a running operation may leave the error its own deadline
should produce; M5's sequence answers with its own `exhausted`, carrying the
server's own sentence, when and only when the server has already refused it
with a "not yet" code. A create that was merely slow is still a preparation
timeout and still says so. Both terminations are reachable on the web now, and
"a late success is released, not attached" is a property of the browser rather
than of the tests. M5's own 60 s watchdog, `sequence.expired` and both its
guards, and the dead half of the late-success fork are deleted;
`openSessionRetryingNotYet` refuses an ownerless sequence outright so nothing
is left unbounded by the removal. `CREATE_RETRY.deadline_ms` stays in the
shared policy: Apple and Android reach it on shipped paths.

**3. "Backoff 1 s · 2 s · 4 s" is the closed list.** *(Ruled; recorded in
implementation §4.6 so it stops reading as an open question.)* Three retries,
four attempts. The text is a closed list, the deadline is still load-bearing
under it — it cuts a slow server mid-attempt — and nothing in the review or the
contract asked for the fourteen-retry reading. What shipped is what was meant.

**4. `segment_503_not_yet` carries its codes.** *(Closed; its own
cherry-pickable commit on `web/playback-surface-reach`, which the Apple and
Android sessions rebase onto.)* The row lists the sixteen 503 codes a playlist
or segment request can actually come back with, read off `http/hls.rs`,
`vodserve.rs` and `http/mod.rs`. On those two resources a 503 is only ever a
"not yet" — the terminal answers are 404/410/502 — so the `recovering` class is
right for every code on the list. `classifyStreamFailure` requires the status
**and** the code for that row, so `vod_disabled` is no longer read as "the
segment is not ready yet", and a code that is also a create code cannot match
off a status that never carried it.

**5. The branch model: task branches into `main`, as an explicit exception.**
*(Ruled; both documents now say the same thing.)* The rule that was followed is
the one that worked, so [AGENTS.md](../../AGENTS.md)'s "large efforts" section keeps
its `effort/<project>` default and gains one bounded exception: a multi-task
project may branch each task from `main` when its implementation plan says so
*and* names the file ownership per task — an effort branch buys serialised
integration, and there is nothing to serialise when no two tasks can touch the
same file. §9 of the implementation plan now says which rule it is exercising
and what earns it.

## 2026-09-13 · The player input fence was red on `main`, on two doc comments

**Fixed in `fix/player-input-fence-comments`.** `scripts/player-input-fence`
matched `onMoveCommand` inside two `///` comments in `LiveTvView.swift` that
explain why the guide grid handles its paging chips itself — prose about a key
handler, with no handler in it. The fence now skips a line only when its
first non-space characters open a comment *and* nothing executable is left on
it — `/* named = */ onKeyEvent { … }`, a `*/` that ends a block and then calls
something, a JS generator method (`*keys(ev)`), a private class field
(`#onkeydown = …`) and an Objective-C `#define` all still trip, and so does a
trailing comment after real code. `fence-fixtures/input/` and
`tests/operations/test_player_input_fence.py` pin each of those shapes and
drive them through the fence's own scanner rather than a second copy of its
loop, so a widened exemption fails here.

## 2026-09-02 · The player input contract, reviewed and finished

**Lane [`effort/player-input-contract`](https://github.com/pjunod/plurx/pull/814),
merged into main, 2026-09-02.** M1–M4 (web #795, Android #797, Apple #799,
fence and settings fold #810) landed first. Each was then reviewed
adversarially against the fixtures rather than against its own description,
which found 29 defects the suites could not see — five of them blockers —
and each became its own task PR into the lane:

- **#825, the gates.** The lane's `check` job had grown an ffmpeg step it has
  no runner label for, so the fast Rust gate was red and the Store contracts
  and the promotion gate cascaded behind it. Two selection gaps went with it:
  `player-input-fence` hung off no client point, so the Kotlin and Swift diffs
  it exists to police never selected it, and a fixture-only diff selected no
  client suite and ran `player-input-contract.test.js` in no job at all — a
  ruling could have been deleted from the contract and merged green.
- **#826, web.** Four producers of playback position became one pending seek:
  a pointer click used to leave a keyboard preview pending (the clock froze
  on a time nobody could see), and a skip's 350 ms debounce could fire over
  the commit that replaced it — or over a player that had already closed.
  `idle` is now an input like any other, so auto-hide stopped keeping its own
  drifting copy of the suppression list, and the second `keydown` listener
  that made every `ignore` row act anyway is gone.
- **#827, Android.** `inputState()` asked a stale focus flag before it asked
  whether the chrome was hidden, so a direction could scrub a player with no
  chrome on screen — the one thing ruling 1 forbids. `reveal` came back on
  Play/Pause every time because `Controls` re-requested its own initial focus
  a frame later. `ignore` was implemented as "not consumed", which handed the
  key to the Media3 session.
- **#828, Apple.** The routing fixture was decoded with
  `.convertFromSnakeCase`, which renames dictionary keys: the contract's own
  input names no longer matched their raw values. tvOS Standard still printed
  a stall-count pill beside the `Stalls` row; focus was sent to a marker
  button that is drawn only while a marker is offered; the invisible reveal
  surface was drawn in front of the failure view, eating every press; and the
  lock screen skipped straight past the reducer.
- **#829, enforcement.** The fence knew one spelling of platform key handling
  (Compose's `Key.` constants, an `onkeydown=` attribute and
  `MPRemoteCommandCenter` all passed) and failed *open* on a missing file.
  Three tables in the contract were hand-written copies of the fixture; they
  are generated now, and the web reads `hide_after_ms`, `skip_seconds` and
  the coalesce window from the fixture instead of repeating them.
- **#831, the rest of the navigation (M5).** Cards and episode rows are
  reachable from a keyboard and a D-pad in every web layout — Classic, the
  default, had no path through the library at all — the header search keeps
  its caret across the re-render its own typing causes, the lightbox and edit
  dialogs announce themselves and return focus, Android's library, search and
  settings screens start with focus somewhere, Search uses the select-to-edit
  field a television needs, and tvOS detail stops re-grabbing focus on every
  appearance. `tests/ui-structure.golden` was regenerated for the new tab
  stops.

Reviewing my own work found five more: a focus request that had become a
value and so stopped moving focus at all, an `ignore` that swallowed
directions on every non-television device, a request that could crash on an
uncomposed node, Home re-grabbing focus on each reload, and Apple's Mini
strip stranded on screen by its own auto-hide fix.

**Since ruled (2026-09-02):** the row grammar now describes what each surface
actually renders — a `bar` row for the corner strip every client puts Close in,
`title_info`/`fullscreen`/`airplay` named as web-only, and desktop's relocated
`info` declared in `surface_placement` — so `player-dom.test.js` derives both
rows from the fixture instead of carrying an exception list. Android's hide
delay and skip step read the fixture too.

**Still open, deliberately not decided here:** Home/End and lock-screen
scrubbing have no contract row, and `touch` cannot reach `scrub` through the
table at all (the pointer drag is prose in `surface_notes`). Nothing here has
run on hardware: Chrome remux, Google TV / Shield, an Android phone, Apple TV
and iPhone are all unclaimed.
