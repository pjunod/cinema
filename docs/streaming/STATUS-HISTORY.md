# Streaming — status history

**Status:** done · records moved verbatim from `STATUS.md` on 2026-09-24

**Moved here from [STATUS.md](../../STATUS.md) on 2026-09-24**, verbatim, by
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
M6. Each section keeps its original heading under the date it was first
recorded in `STATUS.md`; relative links are re-based to this folder and
nothing else changed. Newest first. These are records: a section here
describes the state on the day it was written, and
`tests/operations/test_status_pr_claims.py` keeps holding it to the same
merged-pull-request rule it held in `STATUS.md`.

## 2026-09-20 · Architecture review, revision 3 — Astra's review merged

**[docs/reviews/ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
revised in place again.** Astra's independent review was written against the
first draft; revision 3 keeps revision 2's corrected remedies and merges
everything Astra added that the first draft had not found, each re-verified in
the tree: an unbounded, unkillable scan probe (`scan/probe.rs:190-215`, C12);
decode-fact lookups that hash three executable-sized inputs under a one-permit
gate before consulting the cache and fall back to catalogue facts on any error
(C13); item-detail badges that unpack whole fragment indexes and `stat` every
media path with no deadline (C14); telemetry that spawns a task and a
consistent settings read per event with no bounded queue (C15); DVR fan-out
that writes sinks sequentially with the lock outside the timeout (L10); HDR10
HEVC output limited to software and QSV by design (Q12); no native adaptive
quality and dormant stall-ticket plumbing (§3.8); and two policy tests red on
`main` — `web-policy.test.js:6007` (a stale call count) and
`web-control.test.js:3164` under Node 22 — both reproduced here (§4.8).
Astra's interlace experiment was re-run on this container's ffmpeg 6.1.1 with
identical counts: the current CPU chain emits 90/90 combed frames tagged
progressive. The seek-scratch reservation finding (C16) is a tracking item
for the existing repair. §9 records what was run and what was not. No code
changed.

## 2026-09-20 · Architecture review, revision 2 — after the adversarial assessment

**[docs/reviews/ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
revised in place; the assessment that drove it is
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md)
(210 dispositions, every appendix finding covered).** The assessment's verdict
on the first draft — a useful defect inventory, unsafe as a direct
implementation handoff — was right: several remedies removed the condition
that made the existing code safe. Revision 2 withdraws or rewrites them, each
re-checked in the tree: B-frames via `negative_cts_offsets` (refused by
`vodgen.rs:399`'s landing check — a timeline design item now); copying the
cache-only admin proof onto ordinary auth and replacing the two-phase logout
with a best-effort delete (both weaken acknowledged revocation); the encoded-VOD
SIGSTOP without a stopped→release transition on `Admissions::live_is_waiting`
(a stopped producer yields `Step::Nothing`, so a later viewer would wait
forever); a 60 s TTL on font attestation (re-enumeration exists to catch font
additions under an immutable recipe); heartbeat-derived clock skew (10 s
heartbeats cannot see a 2 s offset) and any clock evaluated inside replicated
SQL; the `NOT EXISTS` search predicate (reproduced to hide renamed titles); a
nonexistent `-hls_start_time_offset`; a self-contradicting release profile;
and Media3's nonexistent `STRATEGY_ALWAYS`. Claims narrowed: the compile-only
fast lane is Paul's 09-10 ruling, not drift — the finding is that the batch
process it assumes has no input; "every audio transcode" → every full video
transcode; rollback artefacts exist as `sha-` image tags, semantic tags do
not; the CI selector run for real gives 3/16 hiqlite and 7/24 SQLite modules
out of scope, not 7/20; the 51 "unindexed" docs are exempted by policy. The
ten do-first items all survive on their code facts; §5 now splits the ones
that were two changes and defers the ones that became design questions.
§0 of the review lists every disposition. No code changed.

## 2026-09-20 · End-to-end architecture review — ten verified do-first items, ranked

**[docs/reviews/ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(the verdict) and its
[appendix](../reviews/ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md) (nine area
reports, ~120 findings with `file:line`).** Nine parallel reviews of `main` @
`a1414368` covering the streaming pipeline, server core, store/cluster, Live
TV, the three clients, build/CI/ops, and the month's git history; every P0/P1
re-verified against the tree before it was written down. The four system-level
findings: the merge gate has run no Rust test since `3cd127e2` (2026-09-10)
and nothing is scheduled after; the hot paths run unsized defaults (4 KiB media
bodies through a blocking-pool hop per chunk, no listener timeouts, encoded-VOD
respawning ffmpeg every control beat, `fc-list` per segment on text burns,
1-pass ABR with `-bf 0`, stereo-only audio, no deinterlacer); the cluster pays
consensus for reads and heartbeats that do not need it and has no backup; and
four files hold 65k product lines at 50–75 % fix density. One live regression
is still on `main`: `process_control::output_job_owned` never pipes the child's
output (`2e3a3bb5`, 2026-09-13), so `dovi_probe_output` always fails — every
Dolby Vision Profile 5 transcode has been refused since the 09-14 deploy — and
`probe_media_origin` always falls back. §5 of the review sequences the work:
twelve small PRs this week, the encoder-defaults set and cluster backup this
month, the `transcode.rs`/`hls.rs`/`vodserve.rs` decomposition this quarter.
Nothing in the tree was changed by this PR.

## 2026-09-17 · A held source is compared on its media facts, not its reporter's schema

**[#354](http://forge.lan:3000/noirr/plurx/pulls/354) from
`fix/probe-reporter-drift` into `main`; adversarial review done (4 blockers, 5
should-fix, 5 notes) and every finding folded or answered.**
Every manual quality change in the web player was refused —
`vod_source_rescan_required`, "could not load stream" to the viewer — on a file
nothing had touched. The VOD recipe re-probes the held source and demanded that
document equal the scan the catalogue stored, and those two documents come from
whatever FFprobe each side ran: the library was scanned by 5.1.9 and the daemon
probes the held descriptor with jellyfin-ffmpeg 8.1.2. Measured on the same
bytes at the same moment, the two builds disagree 32 times and about no media
fact at all — `tags.vendor_id` gone, `tags.name` new, a derived Atmos profile,
`start_time`/`duration`/`bit_rate` estimated to different precision. Probing by
path and by `/dev/fd/3` is byte-identical on both builds, so all of it is the
reporter. **5,061 of the 5,955 files** in that library carry a scan from the
older one. Probe documents now record the build that wrote them; two documents
from the same build are still compared whole, and only a proved difference in
build narrows the comparison — to a declared projection of geometry, cadence,
codec identity and parameter-set layout, colour, every side-data record, the
audio shape, language, disposition, container size and chapter timing, each
compared strictly including when only one document reports it, with a relative
tolerance on container duration alone. Proven against the production documents
themselves: **40 of 40** sampled scan/held pairs are admitted, where `main`
refuses 4. The Developer tab's "Source verification" card now reads the live
provenance and says how many sources this node admitted on the narrower
comparison, advisory only, and `plurx_probe_reporter_drift_admissions_total`
carries the same number. Two rows on that card were stale the moment this
landed ("scan provenance: not recorded", "typed source verification: not
built") and are now live readings. Not done: the scanner still does not
re-probe a file whose stored reporter differs from the running one, so those
5,061 documents stay on the fact comparison until an item is reanalysed —
`POST /api/v1/items/:id/reanalyze` does repair one on demand, which is what the
refusal already tells an operator to do.

## 2026-09-09 · Fresh Dolby Vision recovery converts, and tvOS can arm Live TV starts

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

## 2026-09-08 · A prepared commit hands the viewer a session that is refused from its first request

**Merged into `main` as `4c93ef29`, 2026-09-08, from
[its pull request](http://forge.lan:3000/noirr/plurx/pulls/137).
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

## 2026-09-08 · The third client speaks the protocol, on a platform the server will not use it for

**Merged into `main` as `b266341e`, 2026-09-08, from
[its pull request](http://forge.lan:3000/noirr/plurx/pulls/136). With it,
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
[M6-ANDROID-CLIENT-STATUS.md](../playback-control/M6-ANDROID-CLIENT-STATUS.md).

**If `effort/decoder-selection-recovery` is promoted later, its four Android
commits are now duplicates** — rebase them away rather than merging them
twice.

## 2026-09-07 · Apple viewers were paying for encoders nobody told them about

**Merged into `main` as `9c5e1f9b`, 2026-09-08, from
[its pull request](http://forge.lan:3000/noirr/plurx/pulls/122). Not
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

## 2026-09-04 · Streaming reliability is under end-to-end review and repair

**Effort `effort/streaming-reliability`, started 2026-09-04 from `48615baf`.**
The live progress ledger is
[STREAMING-RELIABILITY-STATUS.md](STREAMING-RELIABILITY-STATUS.md): review
scope, verified facts, task PRs, adversarial findings, test evidence, and the
autonomous decisions made while Paul is away. The effort does not reach
`main` until its final fixed tree has one full qualification receipt.
Current writes, reviews, and merges use Forgejo (`noirr/plurx`); GitHub is a
read-only historical remote. Client-recovery corrections are in local
verification after the first adversarial pass rejected nine correctness gaps.

## 2026-09-03 · The fragment-index queue built nothing for three days

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

## 2026-09-03 · Source attestation was hashing whole films to answer a question about their inode

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

## 2026-09-03 · Dolby Vision Profile 7 on the web — what was actually left

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
their checkouts, while **lab4 ran `v0.3.0-487-gd7194b05`** against a checkout
at 515 — twenty-eight commits of drift, on the node the M5 verification
document names. *That drift is closed:* the deploy recorded below brought all
four to one build, and the version table has been taken out of
`M5-VERIFICATION-PROMPT.md` §1 entirely, because a version written into a
document is stale the day after. lab4's `plurxd` logged no plan-derivation traffic
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

**Deployed to the four servers 2026-09-03, `v0.3.0-568-gd4c67ff4`** — media1,
lab6, lab4, lab3, all healthy, all answering `/readyz`. The first attempt did
not get there: a `deploy.yml` run from an agent session
restarted media1 and was then killed mid-task by that session's own command
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

## 2026-09-03 · Nothing played on the web, and every fallback was terminal

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
exists to recover became a dead player instead. Observed on lab4, file 70.

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

## 2026-09-02 · M7 M4 burn-join and current-main corrections

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


## 2026-09-02 · M7 R-M3 — one playback owns one subtitle window

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


## 2026-09-02 · Apple pacing-hold freeze — the hold that vetoed its own recovery

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
