# Status — what the agent is working on and where it stands

**Updated:** 2026-09-12 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## Live TV gets the web page's proportions on Apple TV and iPhone

**Built for issue #267; iOS and tvOS compile green and the full simulator
suite is green at 968 cases on the lab Mac.** tvOS resolves the semantic text
styles two to two and a half times larger than iOS does — `.subheadline` is
38 pt there and `.title2` 57 — so a channel row ran the width of the screen
and three bands of chrome left the live preview a fifth of it. Live TV now
sizes itself through one explicit scale and spends one 48 pt toolbar row.

The arrangement follows the rule the web page follows: lists are tall and
narrow, grids are wide. On now is a 620 pt column beside a large picture that
is itself a focus target, so Select on it is Fullscreen and "Return to live"
left the toolbar. Guide is a 302 pt stage over a full-width grid whose slot
width is derived from the width the grid actually got — a hard-coded 300 pt
filled 58% of a 1920 pt screen and could never fit two hours — and the fixed
`rows * rowHeight + 54` frame that pushed the last rows off the bottom is
gone. Earlier / Now / Later became chips in the grid header; the status banner
became a channel count plus a four-second toast.

The Layout menu offers Preview and Over picture. A stored `channel_browser`
still decodes and keeps its raw value; it renders as Preview, because the two
only ever differed in which browse view they opened with. On iPhone the
picture is full-bleed with its chips on it, and the six-line now bar, the
always-visible search field and the filter toggles are gone — four to five
channel rows are visible while a channel plays instead of one.

Nothing about playback, the lease, the input contract or enablement changed.
Apple build 146.

## Fresh Dolby Vision recovery converts, and tvOS can arm Live TV starts

**Built for issue #215; focused Rust regressions pass, while the local Xcode
build service is wedged before Swift compilation.** A Profile 7 decision
correctly requested Profile 8.1, but the growing-HLS copy immediately passed
that request through `served_copy_options`, which exists for the legacy muxer
and deliberately narrows unsupported conversion to HDR10. The GOP-aware copy
segmenter now keeps the request, runs the existing post-mux RPU converter
before any segment measurement or publication, writes the matching Profile 8
record into `init.mp4`, and has no legacy retry that could change the frozen
presentation. An end-to-end fixture reads the emitted init and segment back:
the record is Profile 8 with no enhancement layer and a second conversion
refuses because the RPUs already say 8.

Apple TV build 126 failed earlier than its tuner request. Its installed app
container had `Library/Caches` but no `Library/Application Support`; the
restart-safety store's first directory lookup failed and permanently latched
`live_tv_storage_unavailable` for that process. On tvOS the token-free,
90-second marker now uses the supported cache directory, still written before
dispatch and removed only when ownership is known or cleanup succeeds. A real
store round-trip XCTest covers the platform path. Two unrelated web starts
did reach the server during diagnosis and timed out as 503 after 10.4 seconds;
that tuner/reception outcome remains separate from the Apple client refusal.

## The transport-recovery campaign asserted a property the system does not have

**Decided and built; the lane that has been red on `main` and every PR since
`14d0519a` gets an assertion it can pass without losing its teeth.**
`ci / cluster transport recovery campaign` compared every one of its forty
cycles against a single warmed sample with all three margins at zero. The
counts it samples do not sit at a value: sockets and owned async tasks
alternate between two states one recovery apart — one connection and one
owned task per peer, still open when `operation_owns_work` clears — and
threads jitter on their own. Forty cycles needed forty favourable draws,
which is why the failure moved between cycles 1, 4, 6, 9 and 13 and why
`Main promotion gate` failed in two seconds behind it on every PR.

**What it asserts now is an envelope.** The series per node is the warmup
plus the twenty cycles, split into an opening half (cycles 0–10) and a
closing half (11–20); neither the floor nor the ceiling of the closing half
may exceed the opening half's by more than the allowance — zero for sockets
and owned async tasks, two for threads. A leak that starts early adds every
cycle and lifts the closing floor by ten or more; one that starts late puts
most of the closing window above anything the opening half showed — the
closing ceiling is the fourth-highest sample, because the high side is
in-flight residue and the record already shows it wobbling by one, so three
spikes are spikes and four are a trend. The drain visits both of its states
within a few cycles and moves neither edge. The option first written up —
last cycle against first cycle — was rejected on the way: two samples of a
two-state range is the same coin flip, drawn twice instead of forty times.
What still hides is stated, not implied, against the alternating series
the lane samples: a single connection leaked after about cycle 13, a
per-cycle leak that starts after about cycle 15, and a thread leak of one
per four recoveries or slower.

What moved: the sampler no longer waits for a ceiling, only for idle and two
identical samples; the record-time bail is gone; the campaign computes and
prints both bands per node at the end of each role and fails there, with the
whole per-cycle series and each cycle's snapshot source in the log (a leader
that moved between the halves is the one benign thing that looks like a
leak); the offline validator recomputes the envelopes from the cycles and
refuses a hand-written or reordered record. A smoke shorter than nineteen
cycles records and prints its envelope but does not fail on it — two-sample
windows are the coin flip again. Artifact schema is version 2
(`resource_*_envelope_allowance`, `resource_envelopes` per role). 39 focused
tests; the mutations prove a socket, thread or task leaked every cycle, a
leak starting at cycle 13, and one socket leaked at cycle 15 all still fail —
and that the measured drain and jitter pass. Decision document:
[docs/cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](docs/cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md)
§0. The lane's `timeout-minutes` goes from 120 to 240 in the same change: a
voter recovery is ~4.7 min on the two-core `ci-topology` runner, so the
voter half alone is ~100 min, and no Forgejo run has ever reached the
learner half to find that out. Still open: Option C there — making `idle`
mean the transports are released, which is a vendored hiqlite change and
helps everything that reads `operation_owns_work`, not only this lane.

## Phase 3 is buildable today, and the first answer to that question was wrong

**Documentation only, and a decision taken in Paul's absence — overrule it
freely, the reasoning is written down so that is cheap.**
`M6-SERVER-PRIME-HANDOFF.md` §5 asked whether to hold server-side priming for
D6 or narrow `PREPARED_AXIS_SETS` to copy-only transitions. This session
answered "hold for D6", an adversarial review checked the premise against the
code rather than the prose, and it does not survive.

**There is an admitted, receipted, copy-only transition, and the VOD engine
serves it today.** `candidate_request` has three arms that leave a `Copy`
predecessor a `Copy` — `Auto`, `Original`, and a `Manual` ask at the source
height — and `EffectiveSelection` maps `Copy` to codec `source` on both sides,
so no `DeliveryMethod` crossing occurs. A direct-playing session toggling
between Auto and Original therefore crosses `{ResolutionOrBitrate}` **alone**:
in the axis table, receipted by M5.5 on Apple at 20/20, and yielding a `Copy`
candidate that `try_create_with_release_fence` serves, so the
`vod_transcode_unavailable` refusal never fires. The module's own test asserts
it returns `Prepare { ResolutionOrBitrate }`. It is a transition a viewer
makes.

So **phase 3 should be built now and proven against that**, with no new
hardware receipt and no change to the axis table. What stays blocked on D6 is
priming for the transition the fleet actually produces — a copy dropping to a
transcoded rung — and building the copy-to-copy case does not front-run it: it
means D6 lands into working machinery instead of an unbuilt phase.

**How the first answer went wrong, recorded because the shape recurs.** It
enumerated the copy-only transitions as *"`{AudioTrackOrOffset}` and a subtitle
burn removal"* and concluded that neither has a receipt. The handoff's own
option 2, four paragraphs above, says *"audio track, source-height
**Original**"* — and the item quietly dropped is the one carrying the receipt.
The substituted example is unreachable besides: `try_create_with_release_fence`
refuses `subtitle_burn.is_some()` independently of D6, so a VOD session with a
burn cannot exist to transition out of. **Swapping an example for the one on
the list is how a false premise reads as true**, and it is the same family as
the guards that could not fail: the reasoning looked careful and checked
nothing.

## The axis receipt said do not widen, and the fleet widened anyway — correctly

**Documentation only.** `docs/playback-control/M6-AXIS-CASE-RESULTS.md` carries
a live verdict — *"do not widen `PREPARED_AXIS`"*, *"do not build §3.4 on this
evidence"* — and `PREPARED_AXIS_SETS` on `main` contains exactly the pair it
refuses. The code is right, and nothing said so.

**There were two runs on 2026-09-03 and that report is the first one.** It used
a 30 Mbit/s shaping proxy against an 18.183 Mbit/s predecessor: 1.65x headroom,
below the 2.0x floor `headroom_refusal` enforces, so it measured a transition
the server would have declined anyway. The re-run on a 40 Mbit/s link, 2.20x,
came back 20/20 clean, and `0cb370ac` admitted the pair on that. A dated
correction now heads the file, its title and status line say superseded, and
`docs/README.md`'s row says so too — that index is how a reader arrives.

**Two things the correction is careful not to claim.** The throughput floor is
why the run does not bear on *admission*; it is not a root cause for the eight
failures. §8 of that report declines to name one and §4's receipts point
elsewhere — failure tracks the commit boundary rather than the link, and every
failed row records `Pred stalls 0`. And **the receipt for the run that admitted
the pair is not in the folder**: it exists only in `0cb370ac`'s commit message.
The axis table is receipt-driven by design, so that is a gap, named rather than
filled by someone who was not there.

## A prepared commit hands the viewer a session that is refused from its first request

**Merged into `main` as `4c93ef29`, 2026-09-08, from
[its pull request](http://192.168.4.7:3000/noirr/plurx/pulls/137).
Documentation and one test comment; no runtime change.** Found while writing
the plan for M6's missing server phase, and it is about code that has been
merged for days rather than anything new.

`commit_media_session_preparation` advances the playback pointer to the
successor and deliberately does not publish it — the same split an activation
makes, and the store contract is right to make it. What an activation also has
is the caller that finishes the job: `settle_activation_predecessor` completes
its successor's handoff once the exact predecessor accepts terminal control. A
commit has no equivalent, so a committed successor stays at
`MEDIA_SESSION_PUBLICATION_BLOCKED` — and **is refused on both planes from its
very first request**, because `classify_durable_route` answers
`OwnerTransition` for any non-zero fence and `control_owner_refusal` refuses
control on the same predicate. The pointer moves and the viewer gets nothing.

**The obvious fix is a regression, and that was established by building it.**
The publication was written as a caller on the commit path; the whole daemon
suite passed, clippy and rustfmt were clean, and an adversarial pass found what
the suite could not. A committed successor has no local worker, because nothing
primes one. Moving the row off the sentinel puts it into
`owned_media_sessions`, and the lease loop renews only sessions that are
*live*: `take_stale_settlement_candidates` selects exactly the inventory rows
that are not, and `end_media_session_if_owner` ends them `replaced` **and
deletes the playback pointer in the same transaction**. Publishing a workerless
successor would have traded a stalled pointer for a deleted one, seconds after
the commit instead of minutes. The sentinel was the only thing keeping the row
out of that sweep.

So the code change was withdrawn and the finding kept where it will be read:
`preparation_executor_commits_a_staged_successor` keeps its sentinel assertion
and now carries the whole mechanism, including the instruction not to "fix" it
by publishing. Two further requirements the same review established are in the
plan: `PredecessorAcknowledged` may not be asserted on a retired row while the
predecessor's worker is still serving admitted bodies, and a publication that
retries against the Store has to fit inside the client's four-second exchange
budget.

`docs/playback-control/M6-SERVER-PRIME-HANDOFF.md` carries the phase this was
found under, and **the decision it waits on**: ~~the only transition M6 admits
produces a `Transcode` recipe, and the VOD engine refuses every non-`Copy` kind
until the D6 device measurement lands. Priming cannot be built against the
transition the fleet actually produces. Hold it for D6, or narrow the axis set
to copy-only and prove the transaction on those.~~ **That premise is false and
the strike-through is deliberate — it is what this entry said, and what
`M6-SERVER-PRIME-HANDOFF.md` §5 said, until a review checked it against the
code. Answered 2026-09-08 and neither of those:** there is already an admitted, receipted, copy-only
transition the engine serves, so phase 3 can be built now without narrowing
anything. See the entry at the top of this page.

## The third client speaks the protocol, on a platform the server will not use it for

**Merged into `main` as `b266341e`, 2026-09-08, from
[its pull request](http://192.168.4.7:3000/noirr/plurx/pulls/136). With it,
all three client halves of M6 are on `main`.** The work was built on
`effort/decoder-selection-recovery`, where it merged as four pull requests,
and reconciled here against `main`'s restructured seek path, control session
and reporter. It was reviewed adversarially as a *change* rather than as a
move, which is the reason it is worth a line: none of the three defects the
review found is in the feature — all three are in the seam, in pairings that
existed on neither branch. `./gradlew testDebugUnitTest :app:assembleDebug
:app:lintDebug` — the non-Docker equivalent of `make android-test` and `make
android` — green, 519 tests, 0 failures, 0 errors.

**What it deliberately did not do, and this matters more than the port.**
Android's `dual_player_preparation` is `false` and stays `false`. M5.5
measured the tunneled Google TV at 0/3 on the same-codec case, both
`PREPARED_AXIS_SETS` rows *are* same-codec, and Gate A is a hardware claim
frozen per platform in protocol v1 — so the server will not stage a successor
for an Android viewer, and this code does not run. It is written, tested and
dark, against the day a measured device class earns the literal. The lab
re-run on the tunneled Google TV was not taken; no lab access from that
session. See
[M6-ANDROID-CLIENT-STATUS.md](docs/playback-control/M6-ANDROID-CLIENT-STATUS.md).

**If `effort/decoder-selection-recovery` is promoted later, its four Android
commits are now duplicates** — rebase them away rather than merging them
twice.

## Apple viewers were paying for encoders nobody told them about

**Merged into `main` as `9c5e1f9b`, 2026-09-08, from
[its pull request](http://192.168.4.7:3000/noirr/plurx/pulls/122). Not
deployed, and not yet on any physical device.**
Apple is the only platform whose `dual_player_preparation` is `true`,
measured on an iPhone 17 Pro Max and an Apple TV 4K at 20/20 on both cases.
So the server has been staging real successors for Apple viewers all along —
minting an incarnation, taking the actor's one preparation slot, writing a
durable row — and then refusing to mention them, because no shipped client
ever named `prepare_replacement` in `supported_actions`. `prepared_successor`
resolved to `NotRequested`, the client was told `{"type":"none"}`, the
`suppressed` counter moved, and 330 seconds later the deadline reaped a
successor nobody had heard of.

Apple now declares the action and drives the whole transaction: a second
`AVPlayer` that is muted, never on a layer and never asked to play, primed to
the viewer's film position through the successor's own `media_origin_ms`;
`metadata_ready` and `buffer_ready` as it gets there; the item handed to the
authoritative player so the layer, Picture in Picture, the time observer and
every KVO survive the switch; and `committed` carrying the wall clock of the
successor's own first qualifying frame, taken from the item's video output
against the film position the switch happened at rather than from a timer.
Every exit frees the second pipeline and settles the staging — a seek, a
second quality change, an audio change, backgrounding, the player ending —
because a staging left to the deadline costs that session its only
preparation for the rest of its life.

**The finding that changed the shape of the work.** The contract this was
built from says the server half is finished. It is finished as a transaction
and not as a stream: `stage_prepared_successor`'s own comment says
"**Stage only** … nothing produced yet", the third of its eight phases —
*reserve and prime* — is not implemented, and the staged row carries the
blocked publication sentinel, so `classify_durable_route` answers
`OwnerTransition` and **a GET of the successor's playlist is answered `503
media_owner_transition` on every request until the pointer moves.** Commit
does not publish it either. A client built from the sequence diagram would
therefore build a second pipeline that can never become playable and pay for
finding that out on *every* quality change. So a successor that dies before
it was ever playable is taken as evidence about this playback rather than
this attempt, and the asking stops for the rest of it: the viewer pays once,
nothing configures it, and the day the priming phase lands this client uses
it with no change at all. The same rule is now written into the contract for
web and Android, and the contract's §1 is corrected — the code wins.

Also here: the contract and its three per-platform briefs, which lived only
on an effort branch 381 commits behind `main`, move into
`docs/playback-control/`; the caller handoff's claim that Apple and Android
send `observed_download_bps` as null is corrected, because both fill it now;
and Settings → Developer's prepared-handoff card says, per requirement,
whether it is currently met — advisory, gating nothing.

An adversarial review of the branch found nine defects and all nine are
fixed. Three were the kind only a reviewer finds: a commit that reported
`failed` for a successor already on screen — which would have told the
server to abort the session the viewer was watching, and fired on every
quality change made while paused; a commit that settled *whatever staging
was current* rather than the one it was called for, which the server would
have accepted and used to move the pointer to a session nothing displayed;
and a `.alreadySettled` replay that silently swallowed the viewer's tap and
changed nothing at all. The switch is now a critical section nothing may
build on top of, every settlement is named, and the successor's alignment
seek waits for an item that can honour it instead of being dropped on an
item one statement old.

The review also caught the branch excluding VOD on the strength of a
contract paragraph that `main` had already contradicted: `f2fecc98`
populates `delivered_bps` for VOD and says in as many words that its absence
"is what made preparation unreachable on the primary presentation". The
exclusion would have disabled this on the presentation the server had just
enabled it for. Both documents and the Rust doc comment that caused it are
corrected.

**Not proven here:** a directed replacement on real hardware and the
fallback interruption Apple has never measured. Both are operator steps; the
prompt for them is in the pull request. `make apple-build` and
`make apple-test` are green on Xcode 26.6 — 900 cases across the iOS and
tvOS destinations.

**One thing this branch got wrong, recorded with the evidence because the
reasoning is the tempting kind.** `web layout and accessibility` was red on
`main`, and this PR's log carried a Playwright `TargetClosedError` that
matched the one in main's. That match was taken as proof the redness was not
this branch's. The same log also carried `DRIFT 54 structural facts differ
from tests/ui-structure.golden` — the Developer card's advisory rows are DOM
facts and the golden had to move with them — so the merge here happened
without the golden it needed, and `main` stayed red on that lane until the
web half carried both cards' facts in `37ce1e87`.

The lesson is sharper than "a job can fail twice", and the job logs are what
sharpen it. Counted across the runs of that lane retained when this was
written (tasks 3351 to 3723), `TargetClosedError` appears **six** times in the
runs that **succeeded** (3404, 3497, 3531) and **four** times in the ones that
failed (3351, 3368, 3451, 3478, 3723) — it is asyncio teardown noise, printed
by passing runs, and nothing has ever failed on it. Every failure of that lane
was a golden `DRIFT`, including the redness on `main` that was being matched
against: `37ce1e87`'s own message says so.
So the signature was never a failure signature. **Before treating a red lane
as somebody else's, find the line that actually failed the job** — the
`Error`/`FAILED`/`DRIFT` the runner exits on — and check whether it names a
surface this branch touched. Matching an error string that also appears in
green runs proves nothing at all.

## The CI fleet filled up because the bound was behind a flag nobody set

**Merged into `main`; the last of it is `5b30eb92`.** Runners kept running
out of disk. The failure never says so: a runner that fills mid-link reports
`ld terminated with signal 7 [Bus error]`, which is what a miscompile looks
like, and the real `No space left on device` is hundreds of lines further down.

A Forgejo runner serves `actions/cache` out of its own `cache.dir`, and
`forgejo-runner` 13.1.0 has **no eviction for it at all** — its own
`generate-config` offers `enabled`, `dir`, `host`, `proxy_port` and the shared
secrets, and nothing else. Everything written there is permanent. This
repository already owned a bounded alternative for both consumers that write
there — an LRU Cargo cache under a 30 G budget, a named BuildKit builder pruned
to 50 G — and both were gated on `persistent-eligible: true` *and*
`CI_EXECUTION_MODE` in {shadow, accelerated}. The variable has never been set
on this repository. So the condition was false on every job ever run, every job
took the unbounded branch, and nothing in the fleet was bounded by anything.

Measured 2026-09-07 on `gha-m6-general-01`: 118 cache entries, 41 G, every one
created in the previous three days — about 13 G/day on a 78 G disk, which fills
a runner guest in a week. Fleet-wide the cache servers held ~167 G, and nynuc
carried another 101 G of Docker images (4 of 131 in use) and 58 G of BuildKit
cache. ~340 G was reclaimed by hand the same day: Docker build cache and
dangling images on nynuc, m6 and nuc4, and the cache servers of the five idle
runner guests reset index-and-blobs together while each was stopped.

The branch removes both gates — where a cache lives follows the runner, not a
rollout flag — and fixes a second defect the flag had been hiding: the pruners'
reserve was a flat 100 GiB, which is larger than the 78-97 GB Incus guests, so
`healthy` could never become true there and the pruner would have deleted every
cache it was permitted to and failed the job anyway. It is 20 % of the
filesystem now, with a floor that cannot exceed a quarter of it. What a job
still cannot bound, it reports: `scripts/ci-runner-cache-audit` writes the
cache server's size and entry count into every Cargo lane's summary and warns
by runner name past 20 G, because deleting from that directory means stopping
the runner and a job cannot stop the runner it is running on.

That last part is `deploy/runner-janitor/`: a script, a systemd unit, an hourly
timer and a one-command installer, on the same numbers — 20 G budget, 20 %
reserve, graceful stop before any delete. It never resets a working runner,
never leaves one stopped, and refuses any `cache.dir` that is not one; each of
those is mutation-proven. Installed and running hourly on nynuc, rogg16, every reachable
Incus runner guest, and the Lima VM that carries `gha-mbp-linux-arm-01` — which
was holding 24G of its own and gave all of it back on the first pass.
`deploy/runner-janitor/macos/` is the launchd equivalent for
`gha-mba-apple-01`, the one runner with no systemd, and it is installed and
verified there: it read the runner's label and config out of the launchd plist,
unloaded the daemon, reset a 4 G cache and loaded it again, and the runner was
back `idle` in Forgejo twenty seconds later. **The cache is bounded fleet-wide.**

**The band between two bounds, found and closed 2026-09-08.**
`gha-nuc4-general-01` refused two jobs in a row — tasks 3753 and 3780 — with
`::error::gha-nuc4-general-01 is out of disk: 18G available at
/opt/forgejo-runner/_work/<hash>/hostexecutor, need 25G`, identical to the
gigabyte across both. **Not because anything pinned the job there** — that
was this session's assumption and it is wrong: eight runners carry the
`general` label, and `gha-m6-general-02` ran the same lane green with 42 G
free. A runner that refuses in fifteen seconds returns to idle immediately and
is therefore first in line for the next job, so **a full runner starves the
pool precisely because it fails fast**. That is worth its own fix — the
preflight could hold the slot on refusal so healthy runners win the race — and
it is named here rather than built at the end of a long session. The janitor reported that runner healthy the
whole time, and by its own rule it was: **the preflight refuses a job below
25 G and the janitor reserved 20 % of the filesystem, 15.6 G on a 78 GB guest.**
Between those two figures is a band where the fleet turns work away and the
janitor reclaims nothing, and 18 G is inside it. Two bounds, both satisfied,
nobody minding the gap.

The reserve is now `max(20 % of the filesystem, the preflight's own number)`.
A demand the filesystem cannot meet is **reported and then ignored** rather
than chased: capping it to half the disk was tried first and is worse than
doing nothing, because `min(25 G, half)` *is* half on every volume under
50 GiB — the smallest hosts would carry the most aggressive reserve this
script has ever kept and prune hourly forever after a figure the same message
calls unreachable. A host that cannot free enough for a lane is a placement
problem. That keeps the lesson of the original bug in this same function: a
fixed 100 GiB reserve on a 78 GB guest could never be satisfied, so the pruner
deleted everything it was permitted to and failed anyway.

The janitor is installed standalone per host and cannot read the workflow at
run time, so contract tests hold the figures level instead of an import.
**Two of those guards were decorations and the review caught both** — which is
the more useful part of this entry. The first read the `"${DISK_GB:-25}"`
fallback in the step body, and `action.yml` sets `DISK_GB` unconditionally, so
that literal can never fire and the test stayed green while the governing
default moved; it reads `inputs.disk-gb`'s own default now, proven by mutating
it. The second was a "said once per pass" flag set inside a function that is
only ever called as `$(...)` — a command substitution is a subshell, so the
flag never reached the parent, and the assertion counting the message passed
because the fixture had one runner and no Docker. The reporting moved to the
parent shell and the fixture grew a second runner; the sample receipt in
`docs/ci/RUNNER-DISK.md` has said `"instances":4` all along, so one runner was
never the case to test against.

**The band is closed for the default bar and not for every lane:** `disk-gb`
is per-lane and `vod_web` asks for 45 G, which raising `REQUIRED_GB` cannot
cover because 45 G is over half a 78 GB guest. That lane is named as a known
exception, counted rather than merely named — a second lane copying `45` was
the likeliest way another one appears — and the scan no longer misses a value
hidden behind a trailing comment.

**And the loop that reported success while achieving nothing.** The janitor can
free the cache and Docker; the OS, the toolchains and the 30 G Cargo cache are
not its to take. A host whose freeable bytes are smaller than its gap was being
stopped, wiped cold and left short every hour, with `done:` reporting a
successful reset each time. Free space is re-read after a reset now, the
shortfall is said out loud, and `short_after` is in the receipt — gated on a
reset having actually happened, because `reset: 0, short_after: 1` would have
described a runner that was merely busy and sent an operator to re-provision a
machine that is fine. `demand_dropped` counts the filesystems running on the
percentage rule instead of the reserve configured for them, Docker's own volume
included: that path was silent, and it is the one that runs
`docker image prune -af`. A bound that silently cannot be met is the failure
this whole entry is about, and the janitor had two more of its own.

**A wrong fix was built first, and the way it was wrong is the lesson.** The
error message names a path under `_work`, so `_work` was taken to be the full
directory and a reaper for it was written — script, tests, mutations, the lot.
An adversarial review checked the premise instead of the code: `18G available
at .../hostexecutor` is `df` on the **filesystem**, not a size of that
directory, and the preflight's own diagnostic in the same log lists the whole
checkout at about 58 MB (`20M docs`, `19M crates`, `4.6M brand`). The reaper
was withdrawn. What it would have deleted was never the problem, and it could
not have fired anyway — it sat behind the same 20 % reserve that had already
read healthy at 18 G. The measurement is now in `docs/ci/RUNNER-DISK.md` and in
the janitor's own header, next to the instruction to measure `_work` before
writing anything that deletes from it.

**Two things the same review found in code that was already merged.** A failed
`rm -rf` — `EBUSY` on a leftover mount, `EACCES` on another uid's file — aborted
the shell under `set -e` before the restart, and a `RETURN` trap does not run
on shell exit, so the janitor could take a runner out of the fleet with nothing
left to bring it back; the operations doc had listed "a failed delete still
ends with the runner up" as a tested invariant, and it never was. It is now.
And the macOS janitor's quiet window — its stricter half of the idle check, and
the only thing standing between a wrong answer and a SIGKILLed Xcode build —
had never been executed by the suite at all, because the fixture had no `_work`
for it to look at. It has a mutation-proven test now, and a faked `stat`,
because BSD `stat -f '%m'` prints an epoch second while GNU `stat` reads `-f`
as `--file-system` and answers a `File: ...` block that bash then evaluates
arithmetically.

## Settings put the operator on the login page, and the cause was a tombstone

**PR [#101](http://192.168.4.7:3000/noirr/plurx/pulls/101) — merged into
`main` as `a1b1fa34`, 2026-09-07. Not deployed.** Opening Settings
on the fleet returned the sign-in screen. The credential was valid the whole
time.

`cluster_node_removals` and `cluster_node_removal_attempts` outlive the removal
they describe: `finalize_node_removal` deliberately leaves both rows behind as
the durable tombstone, and a trigger protects them. Both guarded acquisitions —
the cache-admin exclusion and the planned-outage lease — read those tables
table-wide, so **the first node a cluster ever removes disables both for the
life of the cluster.** This fleet removed two nodes in August. Since then the
one-time credential-guard activation has retried every three seconds and never
completed (478 refusals in the 24 hours sampled, on each of four nodes, naming
nothing); `cache_admin_revocation_ready` therefore answered false, which fences
the Store-free admin proof cache closed on every node; `/cluster/status`, whose
guard consulted only that cache, answered **401** to an administrator; and the
web client, reading any 401 as "your session is over", called `logout()`.
Settings remembers its last section, so an operator whose last section was
Cluster met the login page on every attempt. `tcpdump` on nynuc confirms the
shape: not one Begin fanout ever leaves the node — every attempt dies at the
lease. `plurx_cluster_removals_pending` correctly reported `0` throughout,
because the gauge already counts only untombstoned removals; the acquire SQL
and the gauge disagreed, and the acquire was wrong.

Four corrections, each with a regression that fails without it: the removal
predicate now means what the gauge means (a removal row whose node is not yet
tombstoned) in both acquisitions and in `lifecycle_operation_pending`; the
cache-only admin guard answers the cached proof first and falls back to a
bounded Store read, so a valid administrator gets `200`, a viewer gets `403`,
an unknown token gets `401`, and a node that cannot find out gets a named
`503` — never a 401 that ends a working session; the web client keeps the
session through a cluster-recovery refusal and paints it in the panel that
reports the cluster; and the activation loop names the precondition blocking
it and backs off to a minute instead of logging one unattributed sentence
every three seconds forever.

An adversarial review then found four gaps in that first tree, all closed
before the merge: the fence was corrected in two of the three statements that
carry it and `enter_maintenance`'s own admission still read the table wide;
the new predicate failed open on a removal row whose target cannot be resolved
at all; the blocker attribution named eight of the acquire's eleven standing
conditions and reported this node's own exclusion as another node's; and the
client guessed rather than asking, so `keepSessionOn401` now confirms the
credential against `/me` before anything is decided — a node still running the
older build refuses these reads with 401 while the session is fine, and a token
that dies mid-tick must still end it.

## A deploy that refused itself over an unmaintainable pair

**PR [#37](http://192.168.4.7:3000/noirr/plurx/pulls/37) merged as
`c661d387`.** `make
docker-up` on nynuc refused to change a container: the health start period was
the tracked five minutes, and `/srv/plurx/plurx.toml` sets
`install_snapshot_timeout_secs = 1200`, which with the three named startup
phases requires 1,335 seconds. The preflight was right. The design was not: the
two halves of that budget live in different files on different machines — the
deadline in a production TOML this repository never sees, the grace in
`deploy/.env` — and nothing paired them, so the pairing rule existed only in
prose and the first report of a mismatch was a refused deploy on the host.

`make docker-up` now derives the grace from the same resolved deadline the
refusal is computed from, proves that value, and applies the value it proved. A
grace an operator wrote is passed through untouched and still refused by name
if it is too short, because a deliberately short grace is how a permanently
broken build gets reported instead of waited out. Verified on nynuc itself:
the same command that refused now reports `health=1335s, snapshot=1200s
(/srv/plurx/plurx.toml)`. `make operations-check` — 185 tests — passes.

## Streaming reliability is under end-to-end review and repair

**Effort `effort/streaming-reliability`, started 2026-09-04 from `48615baf`.**
The live progress ledger is
[STREAMING-RELIABILITY-STATUS.md](docs/streaming/STREAMING-RELIABILITY-STATUS.md): review
scope, verified facts, task PRs, adversarial findings, test evidence, and the
autonomous decisions made while Paul is away. The effort does not reach
`main` until its final fixed tree has one full qualification receipt.
Current writes, reviews, and merges use Forgejo (`noirr/plurx`); GitHub is a
read-only historical remote. Client-recovery corrections are in local
verification after the first adversarial pass rejected nine correctness gaps.

## The fragment-index queue built nothing for three days

**PR [#873](https://github.com/pjunod/plurx/pull/873) — merged as `d90ca299`
into `effort/fragment-index-queue-repair`, 2026-09-03, and being qualified
into `main` now.** Milestones M0–M5.1 of the queue-repair handoff. M0 — the outage fix — is already on `main`: out-of-order `$N`
placeholders in the replicated renew and yield statements, refused by
`validate_parameter_order` before any I/O, so every lease heartbeat failed on
its first tick and every claimed job lost its lease.

Since then, on this branch. **M2**: the row remembers the code every charged
attempt ended with, so a failure keeps its history instead of only its last
line. **M3**: the queue can say in one word whether it is producing, and every
figure the verdict divides by now comes from the same table over the same 24
hours — the first cut divided a 24-hour window by a per-process counter that
reset on restart and was shared with the marker pipeline, so every daemon
restart reported `degraded` and a busy marker pass could vouch for a dead index
queue. **M4**: failed work can be reopened in bulk, previewing first, and only
while nothing for that source is already queued or built — without that rule
the button re-forces an already-indexed library on every press. **M5.1**: a
voter that claims a job whose artifact another voter already published settles
it by hydration instead of rebuilding, which on a four-voter fleet is three
full bitstream passes over each file that nobody needed to run.

**M5.2** is in, on Paul's ruling: background discovery now issues one request
per copy-video identity a file lacks, instead of one request that resolved
whichever identity was missing first and tombstoned the rest. That old shape is
why no converting Dolby Vision index existed anywhere on the fleet — a Profile
7 title got its stripped identity and never its converting one, so only a play
attempt or an admin request could reach the identity that makes P7 play as
graded. It needed `analysis_requests.video_identity` (replicated v27, SQLite
v47) because the forced-successor cancellation has to be scoped to one
identity: `cancelled` is terminal, so a force that cancelled all three would
refuse the siblings' generations for good. **That makes this a stop-the-fleet
deploy** — every node down, upgraded, and back up together.

**M5.4** cannot be measured until M5.2 has run on the fleet.

Every milestone got an adversarial review before merge and every review found
real defects — the M3 and M4 reviews each found a fault that would have
misled an operator in production. The fixes are in the PR comments beside what
they were.

**Wants deploying** once merged, and wants deploying before or alongside the
attestation change: the reopen endpoint is how the rows stranded by the outage
come back.

## Source attestation was hashing whole films to answer a question about their inode

**PR [#877](https://github.com/pjunod/plurx/pull/877) — merged to `main` as
`d0e5c630`, 2026-09-03.** Executes Paul's
2026-09-03 ruling on `DV-P7-ATTESTATION-TIMEOUT-FINDINGS.md` §9. Every
fragment-index build attested its source by reading every byte of it into a
SHA-256. On a 43 GB Dolby Vision title at the fleet's measured 27 MB/s that
is forty-three minutes, and it had to survive both a bare ten-minute deadline
*and* `wait_for_cluster_fragment_index_stop`, which cancels on any playback
admission. Nothing partial is kept when either arm wins — `tokio::select!`
drops the future — so the next attempt started from zero, and five attempts
later the row was terminal on a node it could never leave. No converting
fragment index, so every P7 title fell back to live HLS and HDR10, on every
node, forever.

The digest was never the thing guaranteeing the bytes. `object_version` —
device, inode, size, mtime and ctime to the nanosecond — is taken before the
read and checked again after it, and the scanner's size and mtime are checked
against both. A whole-file hash on top of that only catches a rewrite that
preserved all of it, which userspace cannot produce. So attestation now reads
a bounded sample: 64 one-megabyte extents at deterministic, 4 KiB-aligned
offsets, the whole file below 64 MiB, with the layout — domain token, size,
extent width and count, each extent's offset and length — hashed ahead of the
bytes so no sampled digest can collide with a whole-file one, with another
layout, or with the same file grown by a byte. Still 64 hex characters, so
cache keys, blob headers and every `source_sha256` column are untouched. About
two seconds per file, per node, at any size.

Three consequences shipped with it. The ten-minute deadline now means a hung
mount rather than a large file, and stays charged on both paths — the plan
called for making it uncharged, and an adversarial review showed that an
uncharged retry is *refunded*, which pins `attempts` at one, flattens the
backoff to its base delay forever, and lets never-terminal rows fill the
4,096-row active-request budget every other file needs. The cluster path
reports `source_attestation_timeout` instead of folding a deadline into
`source_attestation_failed`; the code already existed everywhere and that path
simply never emitted it, and an untargeted job that times out is now yielded
*without* the day-long node-local exclusion a genuine refusal earns. And
attestation reports bytes read against the sample it will actually read, so
the `verifying` stage shows real progress instead of three zeros against the
whole file — the daemon's hash rate had never been measured because nothing
ever published it.

The 1,939 rows already stranded at `attempt_limit` are the #700 placeholder
bug's legacy, not the timeout's, and they are terminal in a way that blocks
their own files: `enqueue_analysis_request` refuses a generation that already
exists in **any** state. Non-forced requests now carry the attestation regime
in their generation fingerprint, which moves every non-forced generation once
and lets background discovery re-request the library over successive passes.
Nothing deletes or edits a row; the tombstones stay as history beside their
successors.

Existing observations are invalidated once, deliberately. `object_version`
carries a regime prefix, so every pre-change memo misses and is replaced. The
plan's default was to grandfather them; review showed that is the more
dangerous option, not the safer one — the memo table records no digest regime,
so a node keeping a whole-file digest keeps a *different cache key* from every
node that attested afresh, neither can hydrate the other's artifact, and
because `object_version` never moves on a stable library nothing would ever
heal it. Re-attesting is what sampling made cheap. The 851 already-indexed
artifacts are re-derived under the new keys as discovery reaches them.

**Wants deploying.** After deploy, `MAX(built_at_ms)` in
`cluster_fragment_index_artifacts` should advance within the hour; a second
`pipeline_sha256` appearing there is the converting pipeline being built for
the first time. Working through 5,847 files at `INDEX_MAX_PER_PASS = 4` per
pass on the default fifteen-minute `vod_index_mins` is roughly **two weeks**,
not hours — lower `vod_index_mins` on the fleet if that is too slow to watch,
and read the queue verdict rather than the artifact count while it runs.

## Dolby Vision Profile 7 on the web — what was actually left

**Effort `effort/dv-p7-web-delivery`, merged as `206ab3c3` (#869) on
2026-09-03.** An adversarial
review of the 2026-09-03 remux-refusal diagnosis found its mechanism right and
its fix already merged: #842 (`60e1be68`) closed the no-caps arm's silent
downgrade, and the argv in the diagnosis is from a build the fleet no longer
runs. What is left is four smaller things, one milestone each.

**M0 — correct the record, pin the call sites.** The #840 entry below called
the refused stream a 7→8.1 conversion; it was the *raw* Profile 7 remux from a
pre-#842 build, and no converted stream has ever reached a browser. Two
regressions added, each mutation-checked: the create arm that gives a no-caps
build a review at all (restore the pre-#842 `None` and the session serves raw
Profile 7 again — every review-level test stays green), and the
`served_copy_options` *call* in the live-recovery copy (delete it and the
spawned argv gains `-strict unofficial` and keeps NAL 62/63, which is exactly
the argv the production log carried). The second reads the argv out of a
scoped `tracing` subscriber, because nothing in the crate captured a spawned
command line before. Paul's R3 ruling — leave the preserved-Profile-7
`dvh1.07`-over-`hvc1` inconsistency — recorded in `docs/PLAYBACK.md`, with the
correction the review forced: Android *does* enumerate Profile 7
(`CapsPolicy.kt` maps `DVHE_DTB -> 7`), so the state is reachable from a
dual-layer box. The delivery plays; what it cannot rely on is the master
playlist and the init segment agreeing about the fourcc.

**M2 — the rejection report names both parties.** `stream_rejected` said
"browser refused the remux stream", which is the browser doing exactly what
its own capabilities document promised, and it carried no `session`, so the
server could not join it to the session it superseded. Both rejection paths
now send the join and three facts: the range and Dolby Vision profile the
create response said this session carries, and the profiles this browser
declared. The server prints them, recomputes `caps_mismatch` rather than
believing the client's copy, and keeps them in the stored event's `extra`.

**M3 — the indexer remembers what it could not do.** A `Truncated` or
`Unsupported` build was a log line: the cursor moved on, and the next wrap of
the library spent the same whole-file read — up to thirty minutes of one
node's disk — while `vodserve` answered `vod_index_pending` for a title that
may never have an index. A node-local `fragment_index_outcomes` table now
records the refusal, keyed and invalidated exactly like `fragment_indexes`, so
a replaced file is eligible again with nothing having to notice. Truncated
backs off (30 min doubling to a day) because the per-file budget is a
wall-clock guess; unsupported is terminal because it is a property of the
bytes. The cluster worker records the same row so a clustered node's badge and
background pass know what it found, but its *queue* policy is left alone —
that is `effort/fragment-index-queue-repair`'s, and it is rewriting the lease
and attempt budget this call feeds. The admin badge gained `refused` and
carries the builder's own reason. A review caught the backoff being inert on
every clustered voter: the hiqlite store's injected clock answers in unix
seconds and the deadline is compared against milliseconds.

**What merging main cost, and what it found.** Adding a v45 migration broke
three fixtures that describe an older schema relative to the newest one
rather than by name. Two `sqlite_v43_guard_migration_*` cases called v43
`SQLITE_SCHEMA_VERSION - 1` and downgraded a current database by undoing v44
alone; `populated_v14_import_fixture` builds a current database and walks it
back by hand, and its list stopped at v44. The third one is the interesting
one: it left `fragment_index_outcomes` in place under a `user_version` of
14, so activation replayed the CREATE onto a database that already had the
table, the voter process died inside `select_daemon_store`, and the one-voter
contract reported it as *activation voter exited before ready* — a failure
whose message names neither the migration nor the table. All three now name
the version they mean and drop everything above it. The whole replicated
Store lane passes locally: 120 of 120.

**The deployed-build re-test, as far as it goes.** The fleet was read over
SSH on 2026-09-03. The handoff's premise holds: #842 (`60e1be68`) is an
ancestor of every binary now running, so the arm it closed is closed in
production. At that reading three nodes ran `v0.3.0-515-gc2702f61`, matching
their checkouts, while **nuc4 ran `v0.3.0-487-gd7194b05`** against a checkout
at 515 — twenty-eight commits of drift, on the node the M5 verification
document names. *That drift is closed:* the deploy recorded below brought all
four to one build, and the version table has been taken out of
`M5-VERIFICATION-PROMPT.md` §1 entirely, because a version written into a
document is stale the day after. nuc4's `plurxd` logged no plan-derivation traffic
at all in twelve hours, which is the honest reason the `plan_derivation`
counters cannot be re-tested from the outside: they only move when someone
plays something. The live store is hiqlite; `/var/lib/plurx/plurx.db` was
last written 2026-08-26 and reading it would answer a stale question.

**Not verified on hardware.** The fleet serves this code now (below), but
nothing here has been played from a browser against it.
`docs/streaming/M5-VERIFICATION-PROMPT.md` is the hand-off, and it is gated on ops:
raft membership, then the analysis queue draining, then file 70's converting
identity being built.

**Merged to `main` 2026-09-03 as `206ab3c3`**, all six milestones, with the
`Main promotion gate` green across every one of its 21 jobs. Two late
corrections landed with it: the caps accusation is scoped to a delivery that
actually failed and reads exactly the declared set it prints (#870), and a
tripwire in the fast Rust gate now fails on the commit that appends the next
schema migration (#872) — v45 broke three "wind a current database backwards"
fixtures at once, in three files none of which the appending change touched,
and the worst of them took twenty minutes of `cluster-store-check` to surface.

**Deployed to the four servers 2026-09-03, `v0.3.0-568-gd4c67ff4`** — nynuc,
m6, nuc4, nuc3, all healthy, all answering `/readyz`. The first attempt did
not get there: a `deploy.yml` run from an agent session
restarted nynuc and was then killed mid-task by that session's own command
timeout, leaving the node out of the cluster for forty minutes while the
other three were never touched. What that node did while it was out is worth
recording, because the obvious reading of it was wrong. It looked like a
catch-up budget too small for the backlog — `install_snapshot_timeout_secs`
(120) plus the 45s Hiqlite start timeout, so 165s — with each expiry shutting
Raft down and the node losing ground every cycle: applied 6660079, then
6661534, then 6661534 again while the quorum watermark climbed 6662973 →
6665155. Raising that budget to 1800s changed nothing. Under it the process
sat for twenty minutes with its Raft port listening, its threads parked, and
not one byte written to `hiqlite/`; the leader logged
`AppendEntries … Unreachable … deadline has elapsed` against it the whole
time. **`docker compose up -d` — recreating the container rather than
restarting the process — caught it up in under a second**, and the node has
been healthy since. The wedge was in that container, not in the budget, and
the raised timeout was removed before the deploy.

**The deploy itself avoids the hole it fell into.** `tasks/app.yml` stops the
stack for the pre-deploy database snapshot and only then runs `make
docker-up`, so a cold Rust build happens with the voter down — which is
exactly how a node ends up thousands of entries behind. Each node here was
brought up with the same steps in the same order, with one addition: a
`docker compose build` *before* the stop, so the `--build` inside `docker-up`
is a cache hit. Build time stayed outside the outage and each node was absent
for about twelve seconds — healthy in 5s, `/readyz` 200 in 5s, no restart
during the attempt, no `unreplicated SQLite` or failed migration in the
attempt's logs. `main` moved twice during the rollout (`35a4773b`, then
`d4c67ff4`), so the first three nodes were run a second time; the whole fleet
is on one commit rather than three, and it moved again to
`v0.3.0-575-gaa486f4b` shortly afterwards. That churn is the point: the useful
question about the fleet is whether the four nodes agree with each other and
with their own checkouts, not whether they match a version typed into a
document, and `M5-VERIFICATION-PROMPT.md` §1 now asks it that way.

**That pre-warm belongs in `app.yml`.** It is the difference between a routine
deploy and the forty-minute recovery above, and it is four lines. Ansible
could not run from this session — the linked machine had no
`ansible-playbook` and 3MB of free disk — so the per-node steps were executed
directly over ssh instead; that is a deviation to close, not a new pattern.

## The ✕ on an iPhone could not leave a film

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

## Activity's Now playing row, read as a card

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

## Nothing played on the web, and every fallback was terminal

**[`agent/hls-startup-demand-deadlock`](https://github.com/pjunod/plurx/pull/840),
merged to main as `9646f99f`, 2026-09-03.** Web playback failed on every title
tried, in three separate browsers, while the TV played the same library —
which is what proved it was the server rather than one browser profile.

Every player reports demand `hold` before it has started, because its
`<video>` has never received a byte and is therefore paused. Explicit flow
control obeyed that hold, so a fresh session's producer was suspended at a
target of zero one second after ffmpeg started. No playlist was ever written;
the playlist request the same client was blocked on spent the whole
`PLAYLIST_WAIT_BUDGET` and returned 503; the client reported
`manifestLoadTimeOut` and the viewer was told the server couldn't build the
stream. The client could not say `active` until it played and could not play
until production ran.

The blast radius was every *fallback*: a session that escalates from a refused
remux to a transcode is a fresh session, so a delivery fault that the fallback
exists to recover became a dead player instead. Observed on nuc4, file 70.

A session below `EXPLICIT_STARTUP_FLOOR_SECS` of published media is *starting*,
and starting suspends the demand hold and the time limiter both — exempting the
hold alone leaves the same deadlock reported as `Time`, because
`time_release_threshold` releases below the floor it would be guarding. The
byte limits are never suspended. The grant is latched per session on
publication, so a producer retry cannot renew it.

Two adversarial reviews, five defects found in the fix itself and all fixed:
a vacuous-and-failing new test, two existing tests silently borrowing the
startup exemption, the disk caps not consulted at all inside the publish gate,
the grant renewing on every retry, and the `Time` relabelling above. Full
qualification green; 1601 unit tests.

**Not fixed here, and still open:** the truncated first segment that caused
the escalation — the **raw Profile 7** remux a pre-#842 build served through
live-HLS recovery, which the browser refused with `MEDIA_ERR_DECODE` after
12 KB of a 12.5 MB segment. Not a 7→8.1 conversion: the conversion exists only
on the VOD path, file 70 has never had its converting fragment index built,
and no converted stream has yet been served to a browser at all. Real,
separate, and now costs a fallback rather than a failure. `vod_index_pending`
and
`vod_transcode_unavailable` both fell through to live-HLS recovery on this
file. And the web client reporting `hold` from a player that has never
started is honest to fix at the client too, though the server invariant has to
hold for every client regardless.

## The picker says which machine again

**[`agent/discovery-machine-name`](https://github.com/pjunod/plurx/pull/839),
open into main, 2026-09-02.** Every node of one logical server reports the
same `server.name`, so since `4a7ead78` (2026-08-20) a clustered node
advertised that bare name plus twelve characters of its node id — three rows
of `plurx · 5deeeebc8f39` in the Apple TV picker, on a fleet whose machines
have perfectly good names. It regressed `474e2ee1`, and neither
`deploy/README.md` nor the compose comments ever stopped promising
`m6 · 192.168.1.20`; the discovery companion still runs `uts: host`
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

## The player input contract, reviewed and finished

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

## M7 M4 burn-join and current-main corrections

**PR [#794](https://github.com/pjunod/plurx/pull/794) — merged `a664dfa5`,
2026-09-02.** M7 M4 shipped through the effort train and its first
post-merge repair, then current-`main` CI exposed an ambiguous-success timeout
while confirming a replacement media session. The exact sentinel-guarded
confirmation now uses the bounded idempotent-write path; a zero-row replay is
accepted only after the durable route and current pointer prove the same
activation. Abandonment and ordinary transactions remain non-retryable.

The same qualification work reproduced two existing daemon-harness flakes.
Healthy Activity waves no longer spend 500 ms of the production two-second
peer deadline waiting for synthetic proxy receivers; exact physical-request
counts remain, and a paused unit now proves seven followers serialize behind
one leading physical fetch and are handed its completed snapshot. The summary
must retain node B's two-stream count and the detail read must name node B
among its deliveries even on a wave the summary led, closing an adversarially
found false-green. Activation fixtures hold all three reserved
HTTP/Raft/Hiqlite listeners open together until the set is chosen, so no
fixture can hand the same ephemeral port to two of its own listeners. The reservation is released before the daemon binds, so
it narrows one fixture's own selection rather than sequencing two fixtures.

- [x] Three adversarial reviews found the summary false-green and a rebased
      evidence commit that named a non-ancestor; both findings are fixed.
- [x] The corrective series is split into Store behavior/evidence and daemon
      harness behavior/evidence, with every mapping naming an ancestor.
- [x] **Superseded evidence, kept for the record.** Before the rebase and the
      two review rounds below, the affected-surface profile passed 12 checks
      with 0 failures over full workspace Rust, 118 executed real-cluster Store
      contracts, topology and failure drills, activation 7/7 and Activity 2/2,
      and hosted run
      [33622277315](https://github.com/pjunod/plurx/actions/runs/33622277315)
      passed policy, WAL, daemon, Store, topology and both package lanes. That
      run's only failure was all 107 ffmpeg-backed Rust cases missing `ffmpeg`
      on the GitHub image, against 1454 other passes from the same binary --
      the prerequisite this branch repairs. Eleven commits have landed since,
      so none of those numbers describes the current head; they are diagnosis,
      not merge evidence. Two Playwright-only preflights were skipped because
      Playwright is not installed on the device host and remain runner work.
- [x] The fast Rust lane now provisions pinned major 6 on hosted runners and
      requires the matching `ffmpeg-6` capability on self-hosted runners, with
      the operations contract binding both sides and pinning the step ahead of
      the gate it provisions. Seven self-hosted runners carry that capability
      alongside `high-cpu`, and two other required jobs already select the same
      class, so the label narrows the pool without stranding the gate.
- [x] Integrate #820, #822 and #806 by rebasing onto `main` `b4a1f108`, and
      refresh every SHA-bound mapping to its new ancestor.
- [x] Two independent adversarial reviews of the rebased head both found that
      the ancestry-refresh commit had renamed the four mapping fragments to
      their new ancestors while leaving each fragment's `commits` field on the
      previous generation, so `make history-check` failed on the committed tree
      while passing in the working tree. Fixed, and six further findings
      implemented: the retried confirmation now reserves a recovery window
      inside the owner lease, the zero-row replay is bound to this call's own
      publication boundary, that guard and the durable-pointer proof are pinned
      at the call site, the after-commit fault injection is scoped to the
      activation statement, the summary-led wave now asserts detail retains node
      B's remote-only streams, and the operations contract pins the hosted
      ffmpeg step ahead of the gate it provisions.
- [x] A second pair of independent adversarial reviews of the repaired head
      agreed on two further defects and found three more. The recovery
      reservation was taken only when the whole margin fitted, so it vanished
      in exactly the case that needs it -- the split is now proportional to the
      lease actually left and lives in one named function. Nothing executed
      that reservation, so a source contract now pins it; four mutations were
      checked against that contract and all four turn it red. The embedded
      SQLite twin had not received the replay-boundary proof its replicated
      counterpart got, and the contract now scans both twins. One assertion in
      the paused Activity unit could not be failed by any mutation and is
      gone. The recovery-window mapping claimed a contract that did not yet
      exist and now states what is actually enforced.
- [x] **The branch moved underneath this work twice.** Another session pushed
      a merge of current `main` onto the PR head while the review repairs were
      being verified, and `main` itself advanced four times in two hours --
      #806, then #821/#824/#789, then #823. Rather than force-push over that
      session's integration, this series is restacked onto the pushed head and
      current `main` is merged in. Nothing of the other session's work is
      discarded; every corrective commit on the series took a new identity, so
      all nine evidence fragments are renamed and rewritten in one commit --
      filename and payload together, which is the pairing the first refresh got
      wrong. The one merge conflict was the rolling-producer ownership
      inventory: #823's production shadow task and this branch's Activity
      in-flight fixture each described the step 381 -> 382, so the merged count
      is 383 and both narratives are kept. The inventory audit settles that
      number, not the prose.
- [x] Every gate re-run on this exact merged head: catalog 23 points / 27
      checks / 1297 audited files; history 1234 corrective commits / 737
      explicit mappings; operations 156/156; ownership and routing inventories
      11/11; `git diff --check` clean. Under the pinned `rustc 1.97.1`,
      `cargo fmt --all -- --check`, `cargo check --workspace --all-targets` and
      `cargo clippy --workspace --all-targets -- -D warnings` are clean,
      including `plurx-core` with `cluster-read-cost-validation`, and the fast
      unit lane passes 1574 with 0 failed and 3 ignored. The
      real-cluster Store, daemon-harness and hosted lanes remain the
      qualification run's work; they are not claimed here.
- [ ] Push the re-reviewed exact head to #794, require its full GitHub
      qualification and promotion receipt, merge it, close partial duplicate
      #782 with a cross-link, and verify `main` after the merge.

**Decisions made without Paul (flagged for review):** consolidate #782's useful
activation port reservation into #794, but replace its privacy-only Activity
barrier removal with the complete three-wave repair and deterministic unit.
Close #782 only after #794 is qualified and merged, so GitHub never loses the
visible replacement before the duplicate closes.


## M7 R-M3 — one playback owns one subtitle window

**PR [#830](https://github.com/pjunod/plurx/pull/830) — merged `653569d0`,
2026-09-02.** The subtitle sidecar cache is keyed by span, and a seek storm
produces a run of legitimately different spans, so twenty extractions could be
running for one viewer with every registry in the server believing each of them
correct. A registry keyed by **playback session** now holds the anchor being
extracted, the control sequence that justified it, a way to stop the real ffmpeg
and a way to wait for it to be gone. The same anchor joins; a different anchor
with a strictly newer settled sequence aborts its predecessor **and waits for
that abort to settle** before starting a successor; a different anchor with
nothing newer behind it starts nothing.

The window path no longer detaches its extraction — cancelling the outer warm
used to cancel a waiter rather than the worker, because `ensure_vtt_at` spawned
a second owner task — while the whole-track path keeps its detached,
cancellation-independent contract exactly, since that sidecar is what every
other consumer needs. An abandoned window leaves **no negative memo**: nothing
was wrong with it except where the viewer went, so the next request for that
span is a first attempt rather than a suppressed retry. Once bytes exist there
is no cancellation point at all, and a published sidecar is never removed by
window-flight cancellation.

The slot is held by a **destructor** and release **fences** the session
briefly — a panicking task, a runtime shutting down, or a request that read its
authority a moment before teardown would otherwise leave an owner nothing will
ever release, with a terminal cleanup parked behind it. Session end releases the
owner on both lifecycles: rolling after terminal admission, VOD inside
`detach_reader` once the readers guard is dropped.

Admission reads the client's settled destination immediately before warming, and
refuses a window that lies behind it or beyond the forward reach a client
buffers into. That reach is deliberate: containment alone would refuse the
window immediately *in front of* the playhead at every grid boundary, which is
the exact gap the bridge exists to close, recurring once per span for the whole
file.

Acceptance is at the production boundary — a twenty-seek storm through real
control exchanges and the real subtitle handler, with producers counted by a
drop guard and flights counted under the owner registry, so a superseded
extraction is observed dying rather than assumed to. Not deployed.


## Apple pacing-hold freeze — the hold that vetoed its own recovery

**PR [#803](https://github.com/pjunod/plurx/pull/803) — merged `d9998f6c`,
2026-09-02.** A stalled
Apple client with an empty buffer and fetchable media asked the server what to
do, was told `hold { reason: "time" }`, showed *"The server is pacing this
stream."*, and returned without reopening its player item. In explicit lease
mode the stall itself manufactures that hold — the production target is the
client's runway plus a 30-second reserve, measured from its own frozen buffer
anchor — so the loop had no exit but the viewer backing out. Web had the same
veto behind a manual *Try again*.

The invariant, established server-side and defended on both clients:
**production state is never authority over serving; a client that can fetch
published bytes may reconnect to fetch them, and the only question a hold
answers is why the producer paused.**

- `resolve_action` answers `none` when the same request proves the client is
  stalled, its decoder starved, its runway at or under 10 s, and at least 10 s
  of published media unfetched — one predicate for all seven hold reasons. The
  hold is still reported in `delivery.hold_reason`; only the instruction is
  withheld, counted by its own
  `plurx_playback_control_recovery_withheld_total{reason,platform}` so it can
  never be read as the vocabulary gap `actions_suppressed_total` measures.
- A wedged reopen keeps its rung on both sides: the client drops the stall
  ticket, and `normalize_claimed_request` declines the one-rung descent for a
  predecessor carrying the wedge signature (no completed delivery for 16 s with
  ≥ 10 s published and unfetched). A slow link fails the idle term and keeps
  today's descent. The server half covers Android's ticketed reopens too. The
  reopen keys on evidence rather than on the stall kind because the two
  detectors race for one freeze.
- VOD's serving frontier is now the contiguous materialized run measured from
  the client's own fetched segment; `published_end_ms` counts from segment 0
  and sits behind the playhead after a far seek. The activity page keeps
  reading the old one, which answers a different question.
- Local gate on the rebased branch: 1552 Rust tests, clippy `-D warnings`,
  `cargo fmt --check`, all four `tests/playback` suites, `history-audit`, and
  every `validate --profile commit --staged` check that does not need cargo.
  Three mutation checks — the predicate call, the server's wedge branch, and
  web's supply `return` — each fail exactly the tests that own them. Apple
  build 110 claimed; Swift and Kotlin compile on the self-hosted runners, and
  device acceptance is opportunistic because the AVPlayer wedge cannot be
  induced on demand. Not deployed.


## Everything a read-only cluster member could not do

**PRs [#806](https://github.com/pjunod/plurx/pull/806) (`b4a1f108`) and
[#821](https://github.com/pjunod/plurx/pull/821) (`27e751ef`) — both MERGED to
main, 2026-09-02.** Started from one screenshot: `nuc3`, a committed learner,
showing a fresh heartbeat, zero apply lag and a green *Read worker ready* pill
while the same card said *Not observed · unreachable*. Two independent defects,
both of the same shape — code that asked whether the local node is a voter by
assuming it is.

**#806, the authority check.** `verify_live_activity_authority` required a
committed **voter at both ends** of every internal peer proof: the answering
node and the signer. So a learner answered 401 to every signed peer request —
its own operations-status reply to the Cluster panel, and every media-session
relay, control and abort a learner ingress originated — and every voter refused
a learner's. Meanwhile `learner_route_eligible` publishes exactly that surface
to learners, `operations_peers`/`media_peers` name them as fan-out targets in
both directions, and `docs/cluster/MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md` §2 already
specified the member-scoped predicate here, giving the voter predicate to
membership mutation alone. The implementation was stricter than its own design.
`PeerAuthorityRole` now names the role each surface asks for; activity
aggregation stays voter-only because its directory never names a learner. The
aggregator also stopped folding a non-success response into `unreachable`
alongside a dead socket — 401/403 are `refused`, other statuses `http_error` —
and every observation code reaches the operator as a sentence rather than its
raw identifier. The cluster-check learner scenario now sends a proof in both
directions and was executed fails-first against the old predicate and against a
signer-only revert.

**#821, the placement arithmetic.** `activity_peers` returns the *other* voters,
so the voters a node can see are `peers.len()` plus itself only when it is one.
`remote_rollout_ready` and the shared-cache canary both added that `1`
unconditionally, which on a learner compares `n` against `n + 1` — false for
every roster size. A learner therefore answered 503 to every delegated media
session while still returning eligible offers a voter ingress would rank and
select, could never place one itself, and never published a verified
shared-cache root; a ready learner sitting at zero active streams was the
visible symptom. Both now count the voters this node can actually see.
Separately, `/internal/media/fragment-index/{key}` was missing from the learner
route matrix while `fragment_index_cluster` hydrates from `media_peers()` — a
peer directory pointing at a door the matrix had nailed shut.

Thirteen mutations across the two branches, all caught. `package and smoke
(amd64)` flaked once on a host-wide port collision with a concurrent job on the
same runner (`127.0.0.1:32402 … Address already in use`); re-run, green. **Not
yet verified on hardware** — when the fleet next takes a build, nuc3 should read
*Direct status ready* rather than *Not observed*, and its active-stream count
should stop being structurally zero.

## Artwork repair-fence claim flake (same CI job)

**PR [#753](https://github.com/pjunod/plurx/pull/753) — MERGED to main
(`36da0497`, 2026-09-01).** While running #744's acceptance loop, the other
intermittent failure in `replicated store and topology contracts` fired —
`new leader did not exclusively fence artwork repair: []`, previously seen
on the effort-train qualification. Root cause: `claim_artwork_source_repair`
declines with an explicit `fence: None` when the leader's quorum
acknowledgement is older than 1s at the claim instant; on a loaded runner
that instant can fall in a scheduling gap the drill's own successor proof
already tolerates. The drill now retries only that classified no-op while
the same node reports itself leader in the same term, bounded well below
one repair lease; both exclusivity bails carry the node's raft state and
the durable repair row — CI's first capture of that evidence
(`since_last_ack: 1036 ms`, term stable) confirmed the mechanism and
exposed a self-defeating freshness guard in the first cut, fixed before
merge. Two adversarial review rounds; 21/21 local drill runs green; CI
green (one unrelated `activity proof` flake on the first run, green on
re-run — noted for a future look if it recurs). Also merged today:
[#752](https://github.com/pjunod/plurx/pull/752), the RELEASING.md answer
to the writeup's §7 question.

## Exact-count cluster assertions (the `v0.3.0` release blocker)

**PR [#744](https://github.com/pjunod/plurx/pull/744) — MERGED to main
(`0600459f`, 2026-09-01) after two adversarial review rounds, a green
`make check` + `make cluster-harness-check`, and a fully green CI run
including the release-blocking `replicated store and topology contracts`
job.**

The two exact-count assertions that failed the `v0.3.0` cut (#736) on a
no-Rust-behaviour diff were investigated per the exact-count writeup,
evidence first:

- [x] Root cause proven: the learner drill's own `provider:artwork` lease
      heartbeat (production `acquire_cluster_job`, renew every 30s) commits
      one Raft entry per renewal; any compaction window slower than 30s is
      off by exactly one. Reproduced deterministically; the contaminating
      entry named down to its SQL.
- [x] §4.3 (watermark read appends per call) refuted by experiment — new
      `plurx-cluster-check -- watermark-experiment N` subcommand, 100 idle
      pairs, zero movement.
- [x] Fix, first round: the drill declares its renewing lease in
      `ForceCompaction` and subtracts the lease row's `revision` advance.
      Superseded by the SQL-class accounting below after the next CI run
      showed revision-invisible background entries; in every round the
      assertion stays `!=`, blank/membership entries stay hard failures, and
      nothing is absorbed into slack — tolerance exists only for entries
      attributed to a declared, named class.
- [x] Instrumentation (validation builds only): applied-entry counters by
      payload kind in vendored hiqlite + env-gated per-entry apply log
      (`PLURX_VALIDATION_LOG_APPLIED=1`); both exact-count windows print a
      full accounting line and name contaminating entries in their bails.
- [x] Adversarial review round: clippy blocker fixed; boundary sampling
      made consistent (retried until no entry commits mid-sample); experiment
      argument rejections tested.
- [x] Second background writer caught by the first post-fix CI run: the
      membership heartbeat (one `cluster_node_heartbeat_intents` transaction
      per node per round) plus failed lease-renewal CAS attempts, neither
      visible to the lease row's revision. Reworked to SQL-class accounting:
      applied normal entries are attributed to registered classes at apply
      time, windows declare which classes are legitimate, undeclared classes
      stay hard contamination, and the topology artifact records the
      tolerated count as `window_background_entries` (schema extended).
- [x] Verification: `make cluster-harness-check` green end to end; 10
      consecutive learner-drill runs green with the renewal accounted;
      `make check` green; PR CI fully green (the one red along the way was
      the stale-base mobile-version trap below, cleared by rebasing).
- [x] Merged as `0600459f`. Nothing to deploy: the change is harness and
      validation-only; production `plurxd` compiles none of it.
- [x] Topology's CI contaminant: attributed to the membership heartbeat
      class and tolerated by name; lease-class traffic there remains hard
      contamination, and any unclassified entry still fails with kinds and
      terms named.

**Decisions made without Paul (flagged for review):** no change to
`docs/RELEASING.md` — the writeup's §7 suggestion (one cold-cache CI run
before the tag) is raised in the PR body for Paul to rule on. Trap worth
knowing: while a PR is open, client version bumps landing on main make the
`mobile release version` gate blame the PR through its stale recorded base —
the fix is a rebase, not a bump.
