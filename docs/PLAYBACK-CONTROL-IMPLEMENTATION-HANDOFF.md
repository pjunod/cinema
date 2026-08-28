# Playback control rewrite — implementation handoff

**Updated:** 2026-08-28
**Merged baseline:** `origin/main` at `bab72ce5` (through PR #635)
**Active PR:** [#636](https://github.com/pjunod/plurx/pull/636)
**Active branch:** `codex/playback-control-m4-prepublication`
**Last merged exact head:** `62a4f535756d624f71d295a11f38bd947d85e841`
(PR #626; hosted run `33129200705` green; merge `9063bb1e`)
**Last formally reviewed head:**
`2bb65af3988fe0bba4b2fcfdedf9c47484f94903` (assertion/scanner repair
unanimously approved by Store, lifecycle, and integration reviewers after runtime head
`841e695f` passed its broad review and all 190 owner rows matched;
earlier rejected heads `9121db03`, `9809d553`, `bb69eb7e`, `31e7d5e2`, and
`763c230c`)
**Implementation freeze:** `f02bf5b5a2ed820f99308ad69720bb449bdf3109`
in the disposable clone at `/private/tmp/plurx-playback-control-clone`.
**Validation-evidence freeze:** `95cd9a2e7ac03ff9fa77a8b2276b42850adf89cf`
for regression-history evidence; owner-inventory reconciliation
`31a93198bc14db6fbcb43e7fdd0e9b4d488fcf19`; compile repair
`ef74b41e57bc26eff06a156024b02029c2c2e768`; its mapping
`b1fe8151d566b54a5958a19911743195769aaa86`; assertion/scanner repairs through
`53c823cacc411de41f9d80c06b15f9cce8631ef8`; and their mappings
`fd08622144c64ae9b17d049bcd54cec8b1dacd91` through `53c823ca` on branch
`codex/playback-control-m4-prepublication`.
**Current candidate identity:** the PR tip containing this handoff; PR #636's
body pins the exact full object ID because a commit cannot contain its own hash.
**Runtime review state:** exact runtime head `841e695f` and compile-repair head
`f0c2297c` were unanimously approved read-only. The directly affected rerun
then compiled successfully and ran 641 tests: 606 passed, 30 hit sandbox
local-bind denials, and five found stale inventories/high-water expectations.
Assertion/scanner repairs through `53c823ca` received targeted exact-head
approval in `2bb65af3`. All five deterministic failures then passed by exact
name, and all 30 socket fixtures passed by exact name with loopback access.
**Current implementation:** the candidate assembles actor-owned
prepublication recovery, exact response admission, cancellation-safe process
and resource settlement, authoritative local/remote routing, bounded relay
stream ownership, and per-rendition VOD build/cleanup ownership. The newest
wave makes cluster replacement make-before-break, persists a replay-visible
successor publication fence and the first durable terminal cause, makes
release a publication-fence/Store-cause/exact-projection transaction, and
binds bodyless status, typed error, and EOF publication to exact response
authority. It also actor-fences copy and rolling-cache response publication,
and gives hard node failure an exact replayable takeover CAS with
abort/join-before-stop provisional ownership and a confirmed bootstrap renewal
before durable-ID adoption and seed/publication.
The legacy published-lifetime and copy recovery decisions still remain
disjoint compatibility owners; this cut does not claim their deletion.
**Validation state:** `cargo fmt`, static diff inspection, and static ownership
recounts passed. `make unit` ran exactly once at unanimously approved
`841e695f`; compilation failed before tests on 11 Store errors. After the
approved compile repair, the directly affected `plurx-core` library and Store
contracts compiled and ran 641 tests: 606 passed initially, all five
deterministic failures passed after repair, and all 30 socket fixtures passed
with unrestricted loopback binds. All 641 targeted tests are now accounted
green without a second full-suite run. PR #626's merged baseline
was green in focused Rust 85/85, ownership 7/7, `make check`,
`make cluster-check`, and hosted run `33129200705`.

This is the resumable execution ledger for the playback-control rewrite. Read
it with the detailed
[`M4 watchdog-removal contract`](PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md),
the parent [`protocol plan`](PLAYBACK-CONTROL-PROTOCOL-PLAN.md), and the live
[`project status`](PLAYBACK-CONTROL-STATUS.md). Update this file at every
commit, exact-head review, test gate, merge, and milestone transition.

## 1. Non-negotiable working rules

- Never edit, switch, build, or test the user's checkout at
  `/Users/pjunod/code/plurx`.
- Work only in the disposable clone at
  `/private/tmp/plurx-playback-control-clone`.
- Preserve the unrelated untracked build artifacts under
  `vendor/hiqlite/target/` and `vendor/hiqlite-wal/target/`.
- Use proper commits and PRs. Do not merge around red or skipped required
  evidence.
- For each PR, obtain adversarial approval of one immutable exact head before
  running unit tests. Run the full local unit suite once on that merge
  candidate. If it fails, rerun only failed or directly affected tests after
  fixes; review every behavioral fix and obtain exact-head approval before
  merge.
- Merge only after that one full unit run, required non-unit cluster/integration
  gates, and every required hosted check are green, with no unresolved
  actionable review item.
- Keep this handoff and `PLAYBACK-CONTROL-STATUS.md` truthful. Foundations and
  inactive state are not complete runtime behavior.

## 2. Delivered baseline

The rewrite is not starting from scratch:

- PR #600: end-to-end explicit control-protocol design and adversarial ledger.
- PRs #602/#605: fenced server control plane and passive web reporter.
- PRs #611/#614/#615/#616/#617: bounded rolling actor, explicit demand lease,
  response-commit ownership, exact-attempt delivery ledger, constant-space
  producer ingress, sole OS-child supervisor, and actor-owned terminal events.
- PR #603 and #606/#610: cluster-shared structural fragment index plus durable
  force-analysis queue/status UI.
- PR #618: complete M4 one-deadline/watchdog-removal implementation contract,
  merged at `f9cef83b` after seven exact-head adversarial passes and green
  local, cluster, and hosted gates.
- PR #619: cutoff-safe progress coverage plus a checked whole-module legacy
  owner inventory, merged at `48ea494c` after thirteen exact-head reviews,
  focused tests, `make check`, `make cluster-check`, and hosted CI all passed.
- PR #621: passive producer deadline and cutoff-safe ingress/fencing, merged as
  `8e331672` after unanimous round-10 approval and hosted run `33109297301`
  passed.
- PR #624: bounded shared command/producer sequencing and observation-only
  projection, merged as `dba35f98` after unanimous exact-head review, green
  local full/cluster gates, and hosted run `33118301414`.
- PR #626: immutable decision slot, non-consuming poll contract, and bounded
  passive executor transport, merged as `9063bb1e` after exact-head review,
  focused Rust 85/85, ownership 7/7, local full/cluster gates, and hosted run
  `33129200705`.

The active branch starts from merged #626. Commit `43bd459d` corrected the
response-publication contract before implementation: frozen generation
metadata leaves retry available, attempt media closes it atomically, current
attempt-derived bodyless responses carry an explicit exact-attempt status
fence, actor-derived errors have explicit non-media fences, and the retry recipe carries the exact
presentation-contract fingerprint. One adversarial pass found four defects;
the corrected delta is approved. The later full unit invocation at `841e695f`
stopped during Store compilation before executing any active-branch test.

### Active prepublication review ledger

The pre-commit review used three independent scopes and did not build or run
tests. At that stage, the repaired working tree received targeted approval
with no remaining P1/P2/P3 finding:

- actor review repaired executor-loss cutoff visibility, exact-deadline
  first-media ownership, synchronous publication projection, cancellation-safe
  follower blocking, retry-neutral generation metadata, and move-only handoff
  settlement;
- transcode review repaired immediate-exit retry replay, HEVC/Dolby Vision
  first-media classification, registry-lock scope, bounded confirmed reap,
  abandoned termination replies, cleanup-owner exclusion from global repair,
  and retirement fencing; and
- HTTP review repaired false `404` collapse, stale `503` publication,
  retired-session failure classification, exact streamed EOF settlement,
  range binding, subtitle typed-error propagation, and exact rolling/VOD owner
  fencing.

Those were working-tree approvals, not the immutable merge-candidate approval.
Formal review of `ab3808b1` subsequently found these additional defects:

- P1: routine End/expiry/supersession could return hardware/software admission
  before the exact prepublication child was confirmed reaped;
- P2: the mechanical owner ledger omitted the replyless termination request,
  shared terminal wait, and reap-attempt timeout;
- P2: a same-incarnation publication race could collapse to fatal `404`;
- P2: rolling ETags did not bind Session incarnation/attempt and could validate
  different equal-length successor bytes;
- P2: each playlist reclassification restarted the whole startup wait instead
  of sharing one absolute request budget; and
- P2: actor admission, first-media handoff, commit, and detached EOF settlement
  had no actual lifecycle bound or bounded settlement owner.

The continuation ledger itself was the P3: it still described runtime and docs
as uncommitted. Exact head `e40c56d4` closes every earlier formal item: routine
prepublication retirement transfers child, scratch, and admissions to a
confirmed-reap owner; every new lifecycle action/timer is counted; response
state races stay typed `503`; rolling validators bind incarnation and exact
attempt; playlist reclassification shares one deadline; actor admission,
first-media application, buffered commit, and streamed EOF settlement are
bounded; and the continuation ledger names the real committed heads.

A final moving-tree audit then found that the playlist byte-resolution path
itself still awaited an unbounded actor observation, synchronous flow-control
refresh, registry/storage/segment locks, and fixed polling sleeps. The repair
puts the whole operation under the existing absolute playlist deadline, makes
the HTTP observation command deadline-aware and fail-closed after
cancellation, clamps polling to the remaining time, queues consequential flow
work to the session-owned worker, detaches non-response slide telemetry, and
publishes cached-integrity failure before cleanup can be cancelled.

Formal review of that head then found the remaining blockers: EOF actor commit
and compatibility projection were not one cancellation-safe application;
first-media settlement lacked finite owner capacity; corrupt-cache cleanup
could still be cancelled after publishing failure; master, VTT, segment, and
retired-but-registered outcomes could still become false `404`; and subtitle
preparation plus VOD resurrection escaped their advertised deadlines. Those
findings are repaired. Follow-up integration review found the remaining
retirement-import, stale-status, bodyless-admission, request-deadline,
remote-handoff, and VOD-negative-owner defects. Those repairs were frozen and
committed as `1bee10d9f6f6d427151dbc3587089b367806c30a`.

Formal review of exact `1bee10d9` ran read-only, without builds or tests, and
returned **REQUEST CHANGES**. Actor review found no runtime P1/P2/P3. The
combined rejection ledger is:

- P1: ownerless VOD `Pending`, `Busy`, `ProducerFailed`, `Io`, and `Gone`
  outcomes could publish stale status after another session reattached with
  the same ID;
- P1: `OwnerGone` mapped directly to `404`, erasing both an active remote
  handoff and a durable terminal result;
- P1: relay ingress, transport, and owner handling each minted a new deadline,
  so the owner could publish after the ingress request had been abandoned;
- P1: default copy and rolling-cache delivery bypassed actor publication
  admission;
- P1: VOD resurrection could be cancelled after predecessor detach but before
  successor attachment and registry insertion, leaving neither generation
  durably reachable;
- P2: transformed init bytes retained the original object's strong ETag;
- P2: VOD exact-EOF, touch, and frontier commit could partially mutate before
  cancellation interrupted the rest of the ownership transfer;
- P2: first-media handoff cleared prepublication projection before lifetime
  and cleanup ownership had transferred;
- P2: the global sessions lock remained held across slow VOD supersession;
- P2: the same global lock remained held across an unbounded actor `End`;
- P3: HTTP conditional handling did not implement weak ETag comparison; and
- P3 documentation: the status and handoff still named `0e80c6ba` or
  `e40c56d4` as current instead of recording the frozen `1bee10d9` rejection.

Runtime repair `8cc28bc8134412149829a37a0cfe8ba83f5da795` closed that review ledger:

- every VOD success, miss, error, and tombstone retains exact attachment
  authority through bodyless status admission;
- durable route resolution distinguishes absent, terminal, active-local, and
  active-remote state, and `OwnerGone` performs one bounded authoritative
  reclassification;
- one authenticated absolute resource deadline crosses ingress, relay
  transport, owner routing, and local handling, with per-resource ceilings and
  conservative clock-skew handling;
- VOD resurrection, supersession, and EOF/frontier mutation are
  cancellation-safe ownership transactions;
- copy and rolling-cache publication enter the actor with an immutable
  response contract and a shared monotone failure fence;
- first-media projection cannot clear confirmed-reap ownership before the
  retained lifetime owner is visible;
- VOD transformed-init validators name the transformed representation, and
  conditional GET uses weak comparison; and
- global rolling-session locks no longer span VOD supersession or actor End.

The pre-freeze review then found three more races: Terminal could be
queued before a deadline yet retire the predecessor after the caller reported
failure; durable-ID adoption before retirement's first registry check could
hide the exact predecessor; and the copy watchdog published its actor-visible
failure only after waiting for process reap. The same runtime commit bounds
only Terminal admission before treating End as irreversible, resolves and
removes retirement targets by exact `Arc` across ID adoption, and publishes
the copy failure fence before kill/reap. Focused regressions were added for all
three. Root review also rejected executable clock-skew slack; owner execution
subtracts the full declared allowance, so skew cannot become work after
ingress abandonment. The assembled implementation and documentation were
then frozen as `763c230cc5de98adc7b07f4635016e5d9db06f2a`.

Formal read-only review rejected exact head `763c230c`. The resulting repair
is frozen in implementation commit `031065ab` and falls into three coordinated
tracks:

1. **HTTP, routing, and relay.** Owner-bearing VOD status survives every
   bodyless response; authoritative route reclassification distinguishes
   absence, terminal state, active local ownership, and active remote
   ownership. Public DELETE and internal abort transfer exact cleanup to
   bounded detached settlement owners, preserve the release reason, and fail
   honestly when remote cleanup cannot be confirmed. Relay preparation keeps
   the inherited request deadline, while an admitted response body receives a
   separate 300-second total and 30-second no-progress lifecycle. Rolling
   upgrades omit unknown optional relay headers; an old peer receives neither
   `If-Range` nor `Range` when it cannot preserve the condition safely.
2. **Rolling authority and settlement.** Terminal command deadlines now bound
   admission, not an already-queued irreversible End. Retirement finds and
   removes the exact Session `Arc` across durable-ID adoption, releases the
   global registry before actor or process settlement, and retains child,
   process admission, and scratch ownership until confirmed reap. A detached
   retirement owner and separate bounded scratch-cleanup owner survive caller
   cancellation. Actor authority loss is projected synchronously, and copy or
   cache failure becomes visible before process kill/reap can race response
   authorization.
3. **VOD build and terminal ownership.** Slow rendition preparation moved out
   of the node-wide rendition map behind a per-key single-flight owner. Missing
   init regeneration is admitted through four node-wide slots, reads at most
   8 MiB, has a 15-second budget, and retains the key until its FFmpeg child is
   confirmed reaped. Dormant purge transfers key, child, accounting, directory,
   and identity cleanup to a detached exact owner. Terminal cleanup immediately
   compacts the heavyweight Session graph; a compact exact-owner tombstone
   retains late `410` replay for 60 seconds and is then evicted only after a
   bounded, fail-closed durable-route confirmation.

Focused deterministic regressions were added for those ownership and
cancellation boundaries, but none has run. Validation on the repair candidate
is limited to `cargo fmt`, static diff inspection, and static ownership
recounts. Freeze this documentation as the immutable combined head, obtain
unanimous adversarial approval of that exact hash, and only then open the
one-full-suite test gate.

A final pre-freeze seam audit found one further P1: public DELETE detached its
outer settlement owner but still applied the HTTP deadline directly to the
commit-unknown `end_media_session` future. The repair now elects one exact
per-capability release owner, synchronously tombstones VOD or actor-fences
rolling publication, and never cancels an admitted Store mutation. A Store
error retains a fail-closed publication fence in a 128-entry registry; the
lifecycle loop retries at fanout four with 1–30-second backoff, caches a
definitive terminal route or absence before lifting the fence, and detaches
exact cleanup so lease renewal never waits for reap or peer transport.

The implementation frozen in `031065ab` then closes the replacement,
durable publication, and response-lifetime seams as one transaction:

- **Replacement is make-before-break.** A replacement first admits and starts
  a provisional successor. The Store activation CAS then atomically moves the
  playback pointer and records the exact predecessor as `superseded`; lack of
  provisional capacity fails the replacement and leaves the current player
  intact. A replacement successor is committed with an indefinitely blocked
  `publication_ready_at_ms` sentinel. The first exact post-commit observer
  arms a fresh 372-second boundary, and request routing/idempotent replay
  publish only after an explicit Store transition to `0`. Exact predecessor
  terminal acknowledgement may clear early because it closes future admission
  under the predecessor's distinct capability; already-admitted bodies may
  drain under that old URL. If acknowledgement is unavailable, only an exact
  elapsed-boundary proof can clear the fence. The owner lease loop resumes
  arming/completion after ingress crash or owner takeover.
- **Terminal cause and publication are durable.** `terminal_reason` records
  the first Store terminal decision (`deleted`, `superseded`, `admin_stop`,
  `revoked`, or `replaced`) and later cleanup paths cannot relabel it. Public
  release first installs a cause-neutral local publication fence, then awaits
  the uncancelled Store End, caches the returned exact terminal route or
  definitive absence, and only then projects that durable winner to the exact
  local or remote owner. `DurableReleaseProof` is constructible only for an
  actually ended route or confirmed absence. The shared settlement publishes
  `204 No Content` only after those facts are replay-visible and owner
  projection has acknowledged, or after the admitted-body safety boundary;
  both acknowledgement and boundary completion are exact Store transitions.
  Ended routes reuse `publication_ready_at_ms`: sentinel means the post-End
  fallback is unarmed, one finite value is armed from exact observation, and
  `0` is durable completion. Replays and owner restarts resume that one value
  instead of restarting 372 seconds. Capacity refusal and commit uncertainty
  return `503` while reconciliation remains fail-closed.
- **Response authority is exact.** Body-bearing responses carry the exact
  engine/incarnation and rolling attempt from classification through
  publication. Attempt-derived bodyless `404`, `416`, `502`, and `503`
  outcomes validate the exact status/error owner but deliberately do not renew
  demand or advance a delivery frontier. Streamed media consumes its move-only
  authorization only after all advertised bytes are read and accepted by the
  bounded HTTP-body channel; a fully buffered response commits only after its
  exact body is prepared and immediately before exposure. Cancellation, short
  read, storage error, or abandonment before the final chunk is accepted is a
  non-commit. Independent pumps own local files, relayed network readers, EOF
  authority, and completion permits even while downstream is backpressured.
  Every prepared or streamed body has a 300-second absolute post-admission
  lifetime. Active file/network pumps also have a 30-second upstream/downstream
  no-progress bound; local streams retain one unacknowledged chunk and account
  or commit only after public Body acceptance, while relays retain two. Public
  bodies reject queued bytes after terminal or absolute timeout.
- **Store parity is explicit.** SQLite and Hiqlite schemas, migrations,
  activation/End/CAS paths, route rows, replicated dump/import, and old-schema
  import projections carry `terminal_reason` and
  `publication_ready_at_ms`. Older imports project `NULL` and `0`
  respectively. Cluster node-removal settlement first-writes `replaced`
  through `COALESCE` rather than overwriting an earlier cause.

Formal review of docs-bearing exact head `31e7d5e2` rejected that first freeze:

- fenced activation replay could never pass after the pointer already named
  the successor, while the replicated UPSERT could shorten a renewed lease and
  regress progress before its exact read;
- private remote-start validation admitted only immutable VOD, while takeover
  then rejected VOD, making the Live rolling takeover path unreachable;
- the takeover publication runway omitted ordinary lease-loop work, and
  supervisor cancellation could detach a creation child or stop its worker
  before the child had finished registration;
- stale settlement could select an inventory row before expiry and End it
  after expiry, destroying the active recipe a survivor needed to claim; and
- takeover commit-unknown, Pending, Lost, pin, runway, and adoption paths had
  no test that retained and observed the same move-only worker capabilities.

Implementation commit `52678b96` was intended to close that ledger. Both
Stores added an optimistic exact pointer/immutable-identity replay path that
returned durable lease, progress, sequence, and publication state when it won
the pre-transaction read.
Network worker ingress remains VOD-only, while a separate bounded validator
admits only persisted Live takeover recipes. `TakeoverCreationOwner` keeps the
child handle and exact worker owner together through abort and join. At that
commit, an exact takeover winner was pinned and adopted before its bounded
renewal to a fresh 24-second wall-and-monotonic lease; ordinary owner renewal
returned to 12 seconds. Ambiguous
renewal cleanup retains the settlement through the proposed lease. Stale End
selection requires fresh runway, an active exact owner/epoch and unchanged
lease boundary; valid Live/typeless routes are left active for takeover, and
both Store CAS implementations reject a same-epoch renewal. The production
reconciliation function now runs behind a small I/O/lifecycle seam, and
scripted source-level regressions cover repeated Pending through winner
publication, definitive loss, and runway/pin/adoption failure without a
second state machine. Three independent moving-tree audits approve these
repairs. No dynamic test has run.

Formal read-only review of the resulting docs-bearing head `bb69eb7e` rejected
that freeze too:

- the Hiqlite optimistic pointer read still left a transaction-order race in
  which an old exact activation replay could overwrite a later renewal and
  prematurely expire live authority;
- both expired-route inventories returned only the oldest fixed page, so
  permanently ineligible legacy or malformed rows could starve every later
  takeover candidate;
- copy startup created scratch, and then a child, before any cancellation
  owner could reap both across capability refusal, origin probe, or aborted
  registration;
- the worker was adopted under its durable public ID before bootstrap renewal,
  and a panic immediately after the registry move could leave cleanup pointing
  at the now-empty provisional ID; and
- the lifecycle seam did not exercise claim/replay/read errors and timeouts,
  renewal ambiguity, cache-generation rejection, supervisor unwind, or exact
  creation abort/join ordering.

Implementation commit `21ecd47f` closes that third ledger. Hiqlite suppresses
every exact-current activation write inside the Raft transaction, accepts only
the fresh-write shape or an all-zero replay, and then proves both immutable
route identity and the current pointer; a contract-only pause forces the real
three-voter ordering of stale pre-read, activation, renewal, and stale
transaction. Both Stores expose an exclusive `(lease_expires_at_ms,
incarnation_id)` keyset cursor, while the process retains its scan position
across ticks so skipped rows cannot monopolize the oldest page. Copy startup's
prepublication guard owns scratch immediately, takes ownership of an
`AttemptChild` before the origin probe awaits, transfers it into the Session,
and settles only after manager registration.

Takeover publication now renews the durable route from the provisional
worker's frontier before the durable capability exists. Registry adoption
move-owns `TakeoverWorkerGuard` and updates its cleanup identity synchronously
with the map rename, before any further await or fallible bookkeeping. The
generic settlement/creation seams cover initial claim, replay, exact read,
bootstrap renewal, cache rejection, cancellation, panic, and child
abort-then-join-before-worker-stop. No dynamic test has run.

Formal review of rebased PR head `9809d553` found five additional issues. The
legacy SQLite importer assumed v34/v35 media-session columns one schema too
early; `TerminalCommitRetry` shadowed its Arc with a mutex guard and referenced
a nonexistent guard name; the remote worker's cleanup guard was armed only
after an unbounded shared-cache pin; the activation comments still described
ordinary starts as unfenced; and the status files did not distinguish the
runtime freeze from the exact docs-bearing candidate.

Implementation commit `6f458ea0` closes that ledger. A schema-backed regression
builds genuine v33, v34, and v35 databases with the real append-only migrations,
seeds a media-session row, and reads all 19 importer parameters with synthesized
or preserved fields at the exact boundaries. Terminal retry again retains a
separate Arc and guard. Remote start arms `StartedSessionGuard` immediately
after creation, bounds pinning to the inherited start deadline, and transfers
the armed guard into the confirmation task; error, timeout, cancellation, and
panic therefore cannot leave an unowned registered worker. The activation
contract now documents that ordinary starts and handoffs use the same pointer
CAS. Commit `5bea1ec7` adds append-only mappings for the hashes rewritten by the
rebase and for this repair after GitHub's automatic static preflight stopped at
that missing evidence. That automatic run executed no unit, focused, cluster,
build, or compile lane. No dynamic test has run locally or remotely.

Review of `9121db03` then found that the cleanup task did not own the remaining
replacement guard, the public local cache pin still lacked its placement
deadline, and the forced three-voter stale-replay fixture used a blocked
publication sentinel despite having no predecessor. Commit `f02bf5b5` moves the
replacement guard into the detached cleanup payload through exact worker and
request settlement, bounds the local pin with a retryable timeout, adds a
guard-level hold/release ordering regression, and corrects the fixture to
publication fence `0`. `95cd9a2e` maps that evidence. The immutable PR #636 tip
containing this handoff was reviewed as `5a9e19ae`: Store and lifecycle
approved it, while integration found no runtime defect and requested only the
17 owner-inventory corrections now committed as `31a93198`. The reconciled
head `841e695f` then received unanimous exact-head approval. Its one full unit
run stopped during compilation before tests; `ef74b41e` repaired the 11 Store
errors and `f0c2297c` received unanimous targeted approval. The directly
affected rerun compiled and executed 641 tests, with 606 passing, 30 sandbox
bind denials, and five stale assertion failures. Repairs through `53c823ca`
received targeted exact-head approval in `2bb65af3`; all 35 exact failed names
then passed.

Merged `main` remains behavior-neutral for recovery. The active cut transfers
prepublication transcode startup authority to the actor/executor and removes
the corresponding detached startup fallback. Published-lifetime and copy
owners remain named compatibility mechanisms until their later bounded cuts.

## 3. PR #619 — merged delivery

### Intended slice

PR #619 did two prerequisite jobs without changing recovery decisions:

1. retain a constant-space, cutoff-safe chain of producer-progress evidence;
2. establish a checked baseline inventory of every old recovery owner and all
   task, timer, process-start, lifecycle, and alias shapes that could recreate
   one.

At the merged baseline the actor still receives one behavior-neutral
projection. No legacy watchdog was removed in PR #619.

### Committed implementation

The merged branch contains:

- `ProgressCoverageBatch`, retaining first advancing progress, contiguous
  covered tail/deadline, first gap, latest progress, and latest telemetry;
- a persistent exact-attempt progress watermark across ingress drains;
- rejection of repeated/regressing timestamps as telemetry-only evidence;
- successor-attempt reset with late-predecessor rejection;
- exit barriers that seal preceding progress;
- an interim legacy-owner catalog and validation test;
- synchronized Markdown and HTML status pages; and
- regression-history evidence.

Committed sequence for merged PR #619:

```text
dafb2b9f feat(playback): retain producer deadline coverage
d0667bff chore(validation): map M4 ingress evidence
12ed2ff3 refactor(playback): fence progress coverage baselines
a376727e chore(validation): bind M4 review corrections
205187e9 test(playback): cover successor and owner races
03ee8ff1 chore(validation): bind structural review fixes
8a18a1b5 chore(validation): enumerate process ownership forms
df2389a7 chore(validation): catalog callable construction paths
252f2a72 test(validation): specify structural syntax contract
5824c8cc test(validation): specify wrapped ownership syntax
b918fdfd test(validation): specify callable ownership contexts
61f7d230 test(validation): specify nested callable groups
11e997cd test(validation): specify labeled expression calls
10c95e6d test(validation): distinguish raw keyword owners
```

### PR #621 adversarial review chronology

PR #621 review round 1 at exact head `4f40dc7c32d0ac693505ed714d0d09a79144a9bc`
returned **REQUEST CHANGES**. Seven implementation obligations must be fixed
before validation:

1. Do not claim lifecycle commands already share due-first ordering; command
   publication sequencing is absent, so the deadline result is provisional and
   action-passive until that shared sequence exists.
2. Capture producer facts under the producer-transition and ingress fence.
3. Settle a due deadline exactly once, with explicit disarm/settled state.
4. Rearm only from cutoff-safe physical publication timestamps, never delayed
   observation or telemetry time.
5. Classify every non-success producer exit immediately at its exit barrier.
6. Route exact physical hold/resume acknowledgements through fenced actor state;
   an issued command is not an acknowledgement.
7. Reject duplicate or regressing rearm evidence by exact-attempt identity and
   monotonic coverage, including exit/successor races.

Those seven implementation repairs landed in `a409a194`; `16723536` documented
them and became the round-2 review head. Round 2 returned **REQUEST CHANGES**:

1. a late or coalesced physical-flow acknowledgement could erase an
   already-won provisional due coordinate;
2. flow publication discarded preceding progress instead of sealing it as an
   ordered barrier; and
3. this handoff and both status pages still identified the round-1 head.

Independent passes also required true ingress sequencing for physical-flow
acknowledgements, called out the absent status/Prometheus projection for the
new actor-private deadline state, found dispatch time sampled before two
possibly contended fences, and corrected duplicate-exit metrics.

Commit `77d90a8eb621d687ca3febe6306dbc7f7c651a83` repairs that review. Successful
exact-attempt SIGSTOP/SIGCONT now becomes a bounded sequenced `FlowApplied`
barrier that seals and preserves preceding progress. A full two-slot barrier
queue defers before the syscall, waits for actor-drained capacity, then
re-authorizes the exact attempt under the transition fence. The bounded
`plurx_playback_rolling_producer_flow_deferrals_total` metric records that
backpressure. Dispatch time is sampled only after transition and ingress are
owned, local flow application claims lifecycle expiry first, timely and late
holds retain their distinct meanings, and duplicate exit stays idempotent
after actor delivery. A duplicate removed before actor delivery is instead
counted as coalesced because it never became a rejected actor observation.

Round 3 reviewed exact head
`3827037aac069e5d212c5e1e2861f2461457bfbc` and returned **REQUEST CHANGES**.
The primary and independent passes found five remaining obligations:

1. A full-capacity producer-flow waiter could remain asleep forever when
   control became unavailable or the actor mailbox closed.
2. Unexpected actor-task exit could race after a signal was authorized and a
   flow barrier reserved but before the process syscall, because actor closure
   was not serialized by the producer-transition fence.
3. There was no direct deterministic regression for stale flow revisions and
   stale attempts while the producer was physically held.
4. A same-attempt exit deduplicated before actor drain had ingress accounting
   but no coalesced-or-rejected outcome accounting.
5. This handoff overstated the capacity/re-authorization tests: the original
   low-level ingress test did not invoke the process supervisor or prove
   successor/retirement behavior.

Commit `728abc86888c06fe7ebf420e06b6e5383b6f780a` repairs all five findings.
Capacity waiters now race bounded-capacity notification against retirement
and actor-mailbox closure, every terminal/unavailable path wakes them, and a
closed mailbox fences the control handle fail-closed. An actor-local drop
fence publishes unexpected actor unavailability while holding the same
producer-transition mutex as process signals, so a signal either linearizes
entirely before retirement or is denied before PID access. Process-level
regressions now exercise the real `AttemptChild` supervisor across capacity
exhaustion, successor-attempt replacement, explicit retirement, actor abort,
post-fence denial, and cleanup progress. Direct state tests pin stale
revision/attempt rejection, and pre-drain duplicate exits increment the
coalesced counter while preserving the first exact terminal observation.

Until shared command sequencing lands, the deadline is an observation/proof
surface only and must not trigger recovery retry, kill, replacement, hold,
resume, or another recovery action.

Round 4 reviewed exact head
`ba1280bab3114ed49a09799658db345ffd25c834`. The primary and independent
concurrency passes approved the runtime design and regressions with no
P1/P2/P3 finding. The contract/test/documentation pass returned **REQUEST
CHANGES** for two repository-gate defects:

1. Corrective runtime commits `a409a194`, `77d90a8e`, and `728abc86`, plus
   documentation commits `16723536`, `3827037a`, and `ba1280ba`, had no explicit
   `validation/regressions.d` mapping. Hosted `history-check` confirmed the
   failure and skipped all test jobs.
2. The committed implementation sequence omitted its then-current
   documentation commit `ba1280ba`.

Commit `2e2136b27d25a8943e0422a74ca27246cb65119b` maps all three runtime repairs
to the retained playback-pipeline and validation evidence and records the
then-current three documentation commits as non-runtime. This handoff refresh
adds the missing sequence entries. Because the repair changes the exact head,
round 4's two approvals do not authorize tests on it: exact-head round 5 is
required.

Round 5 reviewed exact head
`4899799846e56b05b56c4a9f4c221a683794ff9e`. The runtime-identity pass proved
the production, playback-test, and ownership-inventory blobs were byte-for-byte
identical to the two runtime-approved round-4 passes. The primary pass returned
**REQUEST CHANGES** for two documentation-ledger defects:

1. The round-4 chronology still said three documentation commits were mapped
   after `d3d5703b` made the mapping contain four.
2. The committed sequence stopped at `2e2136b2` without either listing
   `d3d5703b` and `48997998` or declaring a boundary for the self-referential
   documentation snapshot.

This documentation-only repair names the count at the point it was made and
defines the sequence boundary explicitly. It changes no runtime, test,
ownership-inventory, or validation behavior. Exact-head round 6 is required
before local tests.

Round 6 reviewed exact head
`5ac31a5c193eee75b4f3868a7614bfeb192383c6`. All three passes **APPROVED**: the
runtime/test/ownership trees remained byte-identical to the earlier runtime
approval, the history mapping was unique and complete, and the documentation
boundary was truthful. The focused test gate opened.

The first authorized command was
`cargo test -p plurxd playback_control::tests -- --nocapture`. Compilation
stopped before any test executed:

1. `#[cfg(test)]` was attached directly to an assignment expression, which is
   unstable (`E0658`).
2. Two process-level regressions kept pinned `child.signal(...)` futures alive
   through later mutable `child.kill()` cleanup (`E0502`).

Commit `97916906c774a26242ba417a23c446aca213772d` wraps the test-only assignment
in a stable block and scopes each pinned signal future so its immutable borrow
ends before cleanup. Two independent static passes approved the narrow repair;
`248d675b60dbd92c55bb460f93fe7782ca7889f9` maps it to the retained evidence.
Because the fix changes code, exact-head round 7 is required before the focused
command may be retried.

Round 7 reviewed exact head
`f31945a3027dad0d5640972bed16539785f634bd`; all three passes **APPROVED**.
Authorized focused validation then passed:

- all 63 `playback_control::tests`;
- both `deferred_flow_signal` process-supervisor regressions;
- the actor-task-exit fencing and cleanup regression; and
- the producer-signal/retirement linearization regression.

That is 67 focused tests with zero failures. `make check` then passed the
catalog audit but stopped at `history-check`, before the workspace test phase,
because the subject of metadata commit `248d675b` contained “repair” and was
itself classified as corrective without a mapping. Commit
`aca0b8d2e138f9230258102fdf9143c257440f66` records `248d675b` as
metadata-only and claims no runtime, test, or validation behavior. No gate has
been rerun. Because the exact head changed, round 8 must approve before
`make check` resumes.

Round 8 reviewed exact head
`5a9a62dbfedfcc4af08912c971c4289084781f04`; all three passes **APPROVED**.
The second `make check` advanced through:

- catalog: 22 points, 27 checks, 9,410 audited files;
- history: 1,002 corrective commits and complete current evidence;
- Python policy/contract suites: 121 and 52 tests passed; and
- `cargo fmt --all --check`.

Clippy then stopped under `-D warnings` because three reported dead-code sites
covered four helpers used only by tests: `into_events`, `drain`,
`settle_due_deadlines_at`, and `handle_producer_event`. Commit
`6333b0656bf553ffe1fe7016bb98880df9a01ab8` marks those adapters `#[cfg(test)]`;
production continues through `drain_blocks`, `handle_producer_blocks_at`, and
the due-first cutoff. Two independent static passes confirmed every call site
is test-only and no production path or ownership count changes. Commit
`aa7e12e48218e339e7ad8b247a5dac1db63fb122` maps the correction. The gate has
not been rerun; exact-head round 9 is required first.

Round 9 reviewed exact head
`86502718f12fc4fc0a49c20438aa0777feb572ed`; all three passes **APPROVED**.
Validation then completed:

- 67 focused playback-control and process-supervisor tests passed;
- `make check` passed catalog/history, the 121- and 52-test Python suites,
  formatting, Clippy, and 1,970 Rust/doc tests with three intentional ignores;
- `make cluster-check` passed vendor snapshot/WAL recovery, the serialized
  replicated-store contracts, compacted-growth, membership/leader/learner and
  singleton/serving failure drills, three/four-voter topology, and seven
  activation plus two activity integration tests; and
- hosted run `33106815012` attempt 3 passed policy, fast Rust, WAL recovery,
  cluster daemon, replicated store/topology, mobile version, and the aggregate
  PR validation gate.

Hosted attempts 1 and 2 failed before repository commands because the
self-hosted fast-Rust runner could not unlink a stale root-owned
`target/.rustc_info.json` during checkout. Attempt 3 used a healthy checkout
and the real format/Clippy/unit/SQLite command passed. This final evidence
snapshot changes documentation only; round 10 must approve its exact head and
the latest hosted checks must be green before merge.

### PR #619 adversarial review chronology

Thirteen exact-head reviews ran. No tests were run during them.

1. `d0667bff`: cross-drain repeated timestamps could manufacture deadline
   coverage; latest progress was hidden by first-gap projection; inventory and
   status were incomplete.
2. `a376727e`: structural inventory missed task/timer forms and the
   successor-attempt race regression.
3. `03ee8ff1`: history mapping loop plus low-level process/lifecycle and alias
   gaps.
4. `8a18a1b5`: bare `Command` UFCS, local task/timer aliases, and a noisy
   all-`clone` sentinel.
5. `df2389a7`: comments could offset structural counts; wrapped/turbofished
   callables and grouped/absolute aliases were not pinned.
6. `252f2a72`: nested parentheses and references plus grouped/absolute
   `Command`, absolute type, and `extern crate` process aliases remained.
7. `5824c8cc`: callable-argument groups could be mistaken for transparent
   wrappers; alias wrappers did not allow interleaved references/parentheses;
   and the immediate-next-step handoff text was stale.
8. `b918fdfd`: a greedy outer argument-group match prevented a valid inner
   wrapped invocation from being reconsidered; two chronology/gate sentences
   in this handoff were stale.
9. `61f7d230`: labeled `break 'label` and `for ... in` expression prefixes
   could hide an otherwise valid wrapped invocation.
10. `11e997cd`: a raw identifier such as `r#in` could be mistaken for an
    expression-prefix keyword, manufacturing a structural owner from an
    ordinary callable argument.
11. `10c95e6d`: approved with no P1/P2/P3 findings. The reviewer confirmed
    the raw-identifier boundary, labeled-break and for-in positives, all
    wrapper and alias distinctions, exact 22/33/3 contract sets, source
    routing and counts, documentation chronology, and runtime ingress logic.
12. `8818b82a`: the gate evidence was truthful and implementation unchanged,
    but the immediate continuation list still instructed a successor to push
    the evidence commit that was already on the remote.
13. `11315987`: approved the documentation-only correction, confirmed every
    runtime/validation blob remained byte-identical to the approved
    implementation, and found no P1/P2/P3 issue.

All #619 findings are closed. The final exact head passed focused, full,
cluster, and hosted gates and merged as `48ea494c`.

### Approved implementation head

The candidate:

- lexically removes line and nested block comments, normal/byte/raw strings,
  actual character literals, and balanced turbofish payloads before structural
  counting;
- preserves raw source for legacy-name sentinels;
- iteratively normalizes innermost balanced callable groups so a valid wrapped
  invocation inside an ordinary argument is found, while a group directly
  owned by a preceding callable remains untouched;
- recognizes expression-prefix keywords and `break 'label` token context so
  label and `for ... in` syntax cannot hide a wrapped call;
- distinguishes raw identifiers such as `r#in` from expression-prefix
  keywords so a callable argument cannot manufacture an owner;
- accepts recursively interleaved parenthesis/reference/dereference wrappers
  in forbidden local aliases;
- catches grouped/absolute `Command` import aliases, absolute `Command` type
  aliases, and `extern crate libc|nix|rustix as ...`;
- carries 22 exact structural rows, 33 positive syntax-contract cases, three
  negative ordinary-argument cases, and exact required-ID assertions; and
- has zero real-source structural count mismatches under a read-only static
  count check.

After exact-head approval, the focused ownership-inventory suite passed 7/7;
the focused producer-progress set passed 5/5; `make check` passed validation,
history, operations, formatting, Clippy, and the workspace tests; and
`make cluster-check` passed its vendor recovery, replicated-store, failure,
topology, and daemon-integration contracts.

## 4. Immediate continuation procedure

PR #626 is merged as `9063bb1e`. The current disposable branch is
`codex/playback-control-m4-prepublication`; contract correction `43bd459d`,
runtime `0e80c6ba`, rejected documentation/review head `ab3808b1`, and first
repair head `e40c56d4` are historical commits. Exact head
`2bb65af3988fe0bba4b2fcfdedf9c47484f94903` is the last formally reviewed
head and received unanimous targeted approval after runtime head `841e695f`.
Runtime behavior is frozen in
`f02bf5b5a2ed820f99308ad69720bb449bdf3109`; regression-history evidence is
frozen in `95cd9a2e7ac03ff9fa77a8b2276b42850adf89cf`; owner-inventory
corrections are frozen in `31a93198bc14db6fbcb43e7fdd0e9b4d488fcf19`;
the compile repair plus mapping are frozen in `ef74b41e` and `b1fe8151`; and
the assertion/scanner repairs plus mappings are frozen through `53c823ca`.
The preserved untracked vendor build artifacts remain outside every commit.

Continue in this order:

1. Verify the final docs-bearing candidate excludes both vendor target trees
   and record its exact immutable branch head without pushing it yet.
2. Run required non-unit static, compiler, and cluster/integration validation.
   The one full local unit invocation has already been consumed and all 35
   exact failed names now pass.
3. Push the approved and locally green candidate to PR #636,
   require every hosted check green on the exact final head, merge, and
   continue immediately with the published-lifetime watchdog cut.

Do not skip the adversarial-review gate because an automatically started
hosted workflow happened to be green. The user explicitly ordered adversarial
review before local unit tests, one full local unit-suite invocation only, and
full required verification before merge.

### Active cut boundaries

The active cut owns transcode prepublication and the response/settlement
fences needed to keep that ownership exact:

- `RollingControlHandle::spawn_prepublication_transcode` returns a handle and
  one move-only executor registration; registration must commit before the
  initial producer policy can arm;
- hardware startup has a 12-second actor budget and at most one immutable,
  presentation-compatible retry; software-only startup has a 30-second actor
  budget and no retry;
- generation-stable master metadata crosses actor admission without consuming
  retry; video/subtitle playlists, VTT, init/media objects, ranges, `304`, and
  other media-bearing attempt responses close retry before exposure, while a
  bodyless `416` uses exact-attempt status admission and does not claim first
  media;
- the executor uses a weak Session reference, confirms predecessor reap before
  scratch clear, verifies the clear, admits the exact recipe, installs one
  successor, and acknowledges the immutable decision; failure never recurses;
- the first actor-authorized attempt-media response ends prepublication
  ownership and starts the retained published-lifetime watcher exactly once.
- HTTP and cluster routes must carry exact response ownership through status,
  range, relay, resurrection, release, and EOF settlement; they may not create
  a second recovery verdict; and
- clustered replacement must create the provisional successor before the
  Store CAS retires its predecessor, persist the successor publication fence,
  and clear it only after exact predecessor acknowledgement or the admitted-
  body safety boundary;
- release must keep cause-neutral local fencing separate from the Store's
  first terminal cause, publish exact durable terminal/absence proof before a
  shared `204`, and retain commit-unknown work in bounded reconciliation; and
- VOD per-key build, head regeneration, dormant purge, and terminal compaction
  are lifecycle/resource owners. They may bound or clean work but cannot pick
  a rolling retry or replace a producer.

This cut removes the detached hardware first-segment fallback as a production
decision owner. It deliberately retains `watch_for_stall*`, `SOFTWARE_GRACE`,
`PROGRESS_STALL`, `WATCHDOG_POLL`, `watchdog_active`, `child_transition`,
`replacing_child`, copy fallback/classification, and published-lifetime stall
handling. Those mechanisms must be listed as retained compatibility owners and
must not overlap the actor-owned prepublication interval.

It also retains `PREPUBLICATION_REAP_RETRY` and
`PREPUBLICATION_REAP_ATTEMPT_TIMEOUT` solely as lifecycle cleanup bounds. Each
termination attempt is bounded and the cleanup owner releases the child
transition before waiting to retry. Neither timer can select a recipe, publish
a response, declare playback failure, or replace a producer. Exact-EOF
settlement and bounded HTTP actor/handoff waits likewise remain response and
network lifecycle bounds rather than recovery watchdogs. Streamed responses
reserve one of 256 completion owners before visibility and receive a fresh
five-second settlement deadline at exact advertised EOF. Buffered response
admission/commit and playlist actor observation share their existing absolute
request deadlines; queued commands reject cancellation or expiry before
mutation.

The candidate adds these named lifecycle bounds. Relay bodies have a
post-header 300-second total and 30-second upstream/downstream no-progress
limit, disjoint from request preparation; local file bodies use the same two
bounds. Independent bounded pumps own the file or network reader even during
socket backpressure, so the terminal-projection safety window can outlive
every admitted predecessor response. Public releases and internal remote
aborts each use 128
settlement slots with inherited five-second admission/response budgets; public
remote release attempts are capped at three with 100 ms between attempts.
Missing-init head regeneration has four process slots, a 15-second limit, and
an 8 MiB read ceiling; the exact key remains owned through confirmed child
reap. Compact VOD terminal response owners replay `410` for 60 seconds, after
which maintenance confirms durable terminal state in bounded three-second,
16-way batches before eviction. These limits contain resource or network
failure. None is permitted to select playback recovery.

### Exact retained-owner inventory

| Owner or bound | Current authority | Why it remains | Deletion or replacement milestone |
|---|---|---|---|
| actor `ProducerProgressDeadline` | Selects the sole prepublication retry or failure at the 12-second hardware or 30-second software boundary | FFmpeg cannot report every silent wedge | Permanent approved server progress deadline after M4 |
| `watch_for_stall*`, `SOFTWARE_GRACE`, `PROGRESS_STALL`, `WATCHDOG_POLL`, `watchdog_active` | Published-lifetime and copy compatibility recovery only | Those two recovery scopes have not moved fully into the actor | Later M4 published-lifetime and copy cuts delete the polling/election owners; the actor deadline retains the required silent-wedge bound |
| `child_transition`, `replacing_child`, `begin_child_replacement`, `kill_child_for_replacement`, `install_replacement_child` | Serializes compatibility child replacement and exact resource transfer | Published/copy in-place replacement still exists | Later M4 cuts replace it with actor decision plus exact-attempt supervisor settlement and drive legacy catalog counts to zero |
| `PREPUBLICATION_REAP_ATTEMPT_TIMEOUT` (2 s) and `PREPUBLICATION_REAP_RETRY` (5 s) | Repeatedly requests termination and waits for exact terminal proof while retaining process admission and scratch | A stuck kill/wait must not leak a permit or authorize overlapping children | Remains as bounded process cleanup; later M4 may simplify the compatibility lock, not the confirmed-reap requirement |
| rolling retirement and scratch settlement | Detached exact owner confirms reap; scratch cleanup uses three 5-second attempts separated by 5 seconds, then leaves recovery to startup/maintenance sweep | Caller cancellation and slow filesystem cleanup cannot strand child or accounting ownership | Remains as lifecycle cleanup; later M4 deletes only compatibility replacement inputs |
| first-media and response settlement | At most 256 first-media owners and 256 streamed-EOF owners; publication/settlement calls use the five-second response lifecycle | A visible response must finish actor handoff or fail closed after request cancellation | Remains as bounded response ownership after M4 |
| playlist/segment request chain | One inherited request deadline; segment lifecycle ceiling is 35 seconds | HTTP and storage waits cannot be unbounded | Remains as a network limit, never a producer verdict |
| local and relay admitted bodies | Every admitted prepared/streamed body receives 300 seconds total; active pumps also enforce 30 seconds without upstream or downstream progress. Independent pumps retain one unacknowledged local chunk or two relay chunks, and public bodies discard queued bytes after terminal/deadline | A dead file, peer, unpolled prepared body, or backpressured receiver needs a finite resource verdict; streamed accounting/EOF cannot precede Body acceptance | The finite body bound remains; M8 may replace the relay transport |
| make-before-break publication handoff | Persists an indefinitely blocked successor, arms 372 seconds from exact post-commit observation, and requires an explicit proof-bound Store transition to publishable `0` | Exact acknowledgement closes new old-capability admission and permits intentional drain overlap under distinct URLs; without acknowledgement, ingress restart/takeover must preserve the same conservative boundary | M6 prepared handoff may generalize the transaction; the durable publication fence remains |
| public release and internal abort settlement | Two independent 128-slot pools; five seconds bounds admission/caller wait only. Admitted Store End has no request deadline; commit-unknown results remain in a 128-entry registry and retry at fanout four with 1–30-second backoff. Ended rows durably record unarmed sentinel, one finite fallback, or completed `0`; acknowledgement and elapsed-boundary completion are exact owner/epoch Store transitions. A 372-second terminal-projection boundary covers the 62-second pre-header ceiling, 300-second body lifetime, and margin when the exact peer cannot acknowledge. Public remote cleanup makes at most three attempts with 100 ms retry delay | Capability deletion must outlive caller cancellation, ambiguous Store replies, and process restart while staying fail-closed, replay-visible, and resource-bounded | M8 prepared node handoff may replace retry transport; exact detached settlement remains |
| VOD segment materialization wait | Bounds demand for an immutable segment that is not yet ready | A client request cannot wait forever for absent bytes | Permanent approved VOD progress deadline |
| VOD missing-init regeneration | Four process slots, 15-second budget, 8 MiB read ceiling, per-rendition key held through confirmed reap | Older/incomplete cache entries can lack a usable init object | Later VOD/index hardening may precompute every init and delete this fallback; until then it remains lifecycle-only |
| VOD terminal cleanup and replay | Heavy graph compacts when cleanup completes; compact exact owner replays `410` for 60 seconds; durable confirmation uses a 3-second timeout and 16-way batch/fanout | Late requests need stable terminal truth without retaining process/media graphs | Retention remains bounded; M8 durable ownership may centralize the compact replay record |
| VOD idle/dormant retention | Session idle TTL 300 seconds; rendition dormant TTL 1,800 seconds; detached purge owns child, key, accounting, directory, and identity cleanup | Immutable materialization needs bounded reuse without permanent memory/disk ownership | Remains as cache lifecycle policy, independent of rolling recovery |
| cluster lease and takeover timers | 3-second renewal, 12-second lease, bounded route query/renewal/takeover work | Only durable routing can distinguish a live owner from a dead node | M8 replaces compatibility takeover with prepared handoff; lease expiry remains a permanent failure fence |
| 15-second flow-control repair tick | Refreshes indexes, retention, speed, pacing, and lease projection | Periodic maintenance remains necessary even when recovery decisions are actor-owned | M4 removes recovery authority from the tick; maintenance scheduling remains |

Client startup/stall/reopen timers remain independent owners on web, Apple,
and Android. M5 replaces each platform's overlapping action paths with one
client controller; M6 moves automatic quality, codec, HDR/Dolby Vision, audio,
subtitle, and node changes to prepare/commit/abort; M9 deletes compatibility
`/status` polling and legacy reopen paths.

The next immutable-head review must specifically challenge exact-deadline
response ordering; stable-master contract freezing; same-ID Session ABA;
executor cancellation at every retry transaction step; permit transfer/leak
behavior; make-before-break activation CAS and durable publication-fence
replay across ingress restart/failover; predecessor projection before
successor exposure; first-writer terminal-cause parity across both Stores,
migrations, old-schema import, dump, and node removal; `204` proof under Store
uncertainty, cancellation, panic, pool saturation, and the 372-second safety
boundary; exact status/error/EOF fencing and local/relay body liveness;
terminal compaction and replay; per-rendition key release; and whether any
request or watcher can still make a competing recovery verdict.

## 5. Merged M4 decision-transport slice after command sequencing

The current slice extends the merged passive deadline foundation with one
immutable decision transport and a passive executor scaffold. It preserves the
earlier shared ordering and observability slice:

- bounded actor command and producer-event envelopes share one ingress
  sequence, so commands, publication facts, exits, and physical-flow barriers
  can be compared without inferring order from task scheduling;
- publication-time coordinates remain distinct from delayed observation time,
  preserving the timestamp at which media became available;
- stale producer exits are fenced by exact attempt and shared sequence, so a
  predecessor cannot relabel a successor after replacement or actor teardown;
- a partial, observation-only producer projection is carried through
  `RollingLeaseSnapshot`, `SessionInfo`, and joined telemetry; and
- bounded command/deadline metrics expose activity without unbounded labels.

The active runtime is committed through `4d0a0c0f`. Root static review found
and removed an initial cycle in which the
actor retained a transport containing a sender back into its own mailbox.
Round 1 at exact head `ceb8c74d` returned one approval and two change requests.
It found that timer-only lease expiry did not wake the executor, executor task
loss was invisible, mailbox loss could hide a committed terminal cause, two
test-only enums would fail warnings-as-errors, one `Arc` map would not compile,
and the tests did not prove their liveness claims. Repair `732d3442` adds a
post-cutoff wake, actor-side executor-closure monitoring, a committed terminal-
cause projection and the compile/lint repairs, with authored coverage for
cancellation, retained notification, bounded inbox, timer expiry, executor
loss, actor loss, terminal cause, and weak ownership.

Round 2 at exact head `ff6ba47d45c9c0101e9675b503fcf1efe7aa9410`
returned one approval and two change requests. Actor loss after an executor
poll began could still be reported as an authority fence even though no
terminal transition had committed; a racing terminal transition could obscure
previously observed executor loss; and the timer, actor-abort, and full-inbox
tests still relied on scheduler timing. Repair `91486148` distinguishes
uncommitted actor loss in the private transport, retains committed terminal
truth in a shared projection, settles terminal status without overwriting prior
`lost` evidence, and replaces scheduling assumptions with explicit actor-start
and executor-poll barriers plus standalone retained-Notify and capacity-one
inbox checks.

Round 3 at exact head `c212547ae46e5ee56dbbab6aae50a75ffa58aecc`
returned one approval and two change requests. A stale poll result already
received by the executor could overwrite an actor-committed terminal state,
and the actor poll result had grown a fourth `Unavailable` variant instead of
keeping transport loss private. Repair `ba3a504d` uses atomic conditional
transitions so `terminal` and `lost` are absorbing, adds a deterministic pause
after poll acquisition to prove stale observation cannot regress terminal
state, restores the actor result to exactly Idle/Decision/Terminal, and maps
transport-only unavailability to executor loss. The executor cursor remains
explicitly an observation, not an application acknowledgement, and terminal
transition settles the retained slot. Legacy watchdogs, direct
replacement/action paths, response admission, cleanup, and process ownership
remain active.

The exact-head review at `3708d37981c9f092855e4a6de09bbd6752b52308`
returned two approvals and one defensive change request: although the current
transport cannot produce uncommitted loss after an actor terminal commit, the
low-level `settle_lost` helper could overwrite terminal if future call ordering
changed. Repair `4d0a0c0f` makes both terminal and lost first-winner absorbing
and directly proves both settlement orders. Only targeted confirmation of that
exact repair remained before tests. All three reviewers subsequently approved
exact head `981ebb51b9d54c5fb71e49232722693e7cf079fe` with no P1/P2/P3 findings.

The focused playback-control suite then passed 85/85. The separate ownership
inventory ran seven tests and exposed six bookkeeping failures: the new tests'
namespaced `RollingControlHandle::spawn`, `Barrier::wait`, and timeout syntax
changed normalized structural counts; the replaced spawn shape changed the
method count; and rustfmt made the decision-poll entrypoint signature multiline.
The test build also warned that deliberately exhaustive passive decision-reason
variants are not all constructed yet. Repair `73cd33e9` records the exact
parser-observed counts and makes the intentional dead-code allowance apply to
test builds. Two targeted reviewers approved it. The Rust suite then passed
85/85 again without warnings, and the inventory rerun cleared all five count
failures but showed the remaining transport entrypoint row could never match:
the entrypoint table reads only `transcode.rs`, while the actor poll correctly
lives in `playback_control.rs`. Repair `9d100a04` removes that misplaced row;
the whole-module decision transport, wake, inbox, and poll sentinels remain.

Targeted review approved exact head `021a44ee`; the ownership inventory then
passed 7/7. The first full `make check` reached the history gate and stopped:
commits `ff6ba47d`, `c212547a`, `f447aa1d`, and `021a44ee` use corrective
subjects, so the chronology checker requires those mapping commits themselves
to appear beside their runtime commit in regression evidence. The existing
four records now include those exact IDs. This is a history-only repair; no
runtime, test, owner count, or behavior changed.

Targeted review approved that chronology at `db19670d`. The next `make check`
passed catalog, history, 121 validation tests, and 52 additional tests before
Clippy found one production-only lint: `RollingControlHandle.decision_transport`
is not directly read outside tests. The field is nevertheless the required
strong lifetime owner for the executor's weak transport reference; removing it
would recreate the premature transport teardown that the slice explicitly
tests. Repair `f463d4d4` documents that invariant and applies a field-local
dead-code allowance.

Two reviewers approved the ownership/lint repair; one identified a stale hash
in this handoff, and the one-line correction received final approval at exact
head `1a0c48cd2e4591a12c9869468031b9a8cf044a73`. The rerun of `make check`
passed catalog/history, Clippy with warnings denied, the full workspace suite,
and doc tests. `make cluster-check` then passed vendor WAL/recovery,
replicated-store, topology, failure-drill, activation, and activity integration
contracts. PR #626 is open and hosted run `33127652859` passed every selected
job plus the aggregate validation gate on reviewed head `1ead3944`.

`ProducerDecision` is now a typed, test-installable actor value and the
executor polls it without consuming it. No production decision is emitted,
there is no `DecisionApplied` acknowledgement, retry cutover, or transparent
handoff in this slice; the v1 wire action remains `none`. The move-only inbox
is registered before `BeginProducerAttempt` can arm its deadline, and its one
request-independent task uses weak transport ownership. A deterministic
regression asserts that dropping the final external handle closes the actor
promptly rather than waiting for lease expiry.

The merged passive deadline still retains its exact armed instant in a
provisional action-passive due record, applies exact-boundary progress or
non-success exit before cutoff, gives lease/session terminal state priority,
and disarms after one observation so it cannot spin. Producer cutoff captures
under the synchronous transition plus ingress fence and folds once; the sole
process supervisor publishes successful exact-attempt SIGSTOP/SIGCONT as
ordered ingress barriers before releasing that fence. Full bounded ingress
waits before the syscall and re-authorizes after actor drain; intentional holds
disarm the clock and a resume grants a fresh full budget. No recovery behavior
consults the passive observation yet.

Primary implementation sites are:

- `crates/plurxd/src/playback_control.rs` for the bounded command envelope,
  actor/producer sequence merge, publication-time ordering, stale-exit fence,
  producer projection, and bounded metrics;
- `crates/plurxd/src/transcode.rs` for the additive `SessionInfo` projection;
- `crates/plurxd/src/http/system.rs` for joined server telemetry; and
- the existing producer-ingress and process-supervisor paths for the passive
  deadline and physical-flow barriers.

The passive deadline implementation remains centered in
`crates/plurxd/src/playback_control.rs`: `ProgressCoverageBatch` and
`RollingProducerIngress`, `RollingControlActor` producer state,
`begin_producer_attempt_at`, `observe_producer_progress_at`,
`observe_producer_exit_at`, `handle_command`, and actor `run`.

Focused evidence passed for command/producer sequence ordering,
publication-time barriers, stale-exit/successor fencing, projection
serialization, bounded metric labels, exact starting expiry and
idempotent settlement, scheduler-delayed dispatch, rearm from fenced
publication time, late/duplicate progress, exact-boundary progress and
non-success exit, contiguous versus gapped A/B/C coverage, capture while
blocked on the transition fence, stale-exit/successor-progress ordering,
successful-exit classification and actor-delivered duplicate rejection,
pre-drain duplicate coalescing, stale physical-flow revision/attempt rejection,
progress sealed around
ordered physical hold/resume barriers, timely versus late hold, capacity
deferral/wakeup through the real process supervisor with exact-attempt
re-authorization against successor and retirement, unexpected actor-exit
serialization, post-fence signal denial, cleanup progress, exact
lifecycle-expiry precedence, process-owner flow publication, and lifecycle
commands before/at/after the provisional producer due coordinate. These are
observation and ordering regressions only; no test claims that the new branch
has completed the decision/action cutover.

Committed implementation and review-repair sequence through the final runtime
head preceding this documentation snapshot:

The documentation commit containing this list and any later metadata-only
binding are intentionally resolved with `git log`; a commit cannot list its
own content-derived hash. They do not alter the candidate behavior described
here.

```text
6b602d6c feat(playback): record actor producer deadlines
dbf12d87 chore(validation): map passive deadline evidence
4f40dc7c docs(playback): bind passive deadline PR
a409a194 fix(playback): fence passive producer cutoff
16723536 docs(playback): record passive cutoff review repairs
77d90a8e fix(playback): sequence physical flow barriers
3827037a docs(playback): record flow barrier review repairs
728abc86 fix(playback): fence deferred flow on actor exit
ba1280ba docs(playback): record actor exit review repairs
2e2136b2 chore(validation): bind actor exit review evidence
d3d5703b docs(playback): record history review repairs
48997998 chore(validation): bind history review evidence
40160841 docs(playback): define review ledger boundary
5ac31a5c chore(validation): bind ledger boundary evidence
97916906 fix(playback): compile deferred flow regressions
248d675b chore(validation): map deferred flow compile repair
c4d01713 docs(playback): record focused compile repair
f31945a3 chore(validation): bind focused compile evidence
aca0b8d2 chore(validation): register compile evidence
db158bd6 docs(playback): record focused gate evidence
5a9a62db chore(validation): bind focused gate evidence
6333b065 fix(playback): gate test-only deadline helpers
aa7e12e4 chore(validation): register deadline helper evidence
6f9676a5 docs(playback): record clippy gate repair
86502718 chore(validation): bind clippy gate evidence
383d62d8 feat(playback): sequence actor command ingress
de23104d chore(validation): map sequenced command ingress
```

The original command-sequencing and projection runtime is committed at
`383d62d8`, with its regression-evidence mapping at `de23104d`; subsequent
review and static-gate repairs culminate in approved implementation head
`dc7c7667`. That head owns the focused, `make check`, and `make cluster-check`
evidence recorded above. The exact final PR head
`12083a5991d27fa88c72c03f344ee5e3939382bb` passed hosted run `33118301414`
and merged as `dba35f98`; that evidence belongs to #624, not the active slice.

Leave all compatibility owners unchanged and active in that slice:
`FIRST_SEGMENT_GRACE`, `SOFTWARE_GRACE`, `PROGRESS_STALL`, `WATCHDOG_POLL`,
`watch_for_stall*`, `playlist_producer_failed`, `downgrade_one_step`,
`child_transition`, `watchdog_active`, and `replacing_child`. The passive actor
deadline observes and records first; later slices move action authority and
delete legacy owners without two concurrent recovery decision makers.

## 6. Remaining roadmap

After the active prepublication candidate, the remaining work is:

1. Satisfy non-unit cluster/hosted gates and merge the production
   decision, prepublication retry, response-admission, relay/VOD ownership,
   and confirmed-reap cut.
2. Move published-lifetime stall classification and recovery behind the actor
   without overlapping the retained compatibility watcher.
3. Convert copy startup/exit classification and fallback to actor decisions.
4. Add desired physical hold/resume command/intention barriers to the shared
   actor sequence; retain the successful physical acknowledgements already in
   producer ingress.
5. Delete detached recovery loops, request-side exit verdicts, in-place
   replacement, `child_transition`, `watchdog_active`, and `replacing_child`;
   drive every legacy catalog sentinel to zero.
6. Complete M5 client action ownership for web, Apple, and Android.
7. Implement prepared/commit/abort handoff for transparent quality,
   resolution, codec, HDR/Dolby Vision, audio, subtitle, and node changes.
8. Extend the shared index with exact intro/credits annotations, subtitle
   windows, force-analysis controls, queue/current-work visibility, and
   instrumentation.
9. Add clustered rolling/VOD takeover and planned drain.
10. Complete mixed-fleet cutover and delete compatibility polling paths.

## 7. Completion definition

The project is complete only when the explicit control protocol owns delivery
state and all recovery actions end to end; automatic media/node changes use a
prepared handoff; intro/credits and subtitle analysis are indexed and
observable; clustered takeover is fenced; all intentionally retained
deadlines are named in the watchdog ledger; the old overlapping watchdogs and
in-place replacement machinery are absent; client and server matrices pass;
and the status page reports no partial milestone as complete.
