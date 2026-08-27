# Playback control rewrite — implementation handoff

**Updated:** 2026-08-27
**Merged baseline:** `origin/main` at `48ea494c` (PR #619)
**Active PR:** [#621](https://github.com/pjunod/plurx/pull/621)
**Active branch:** `codex/playback-control-m4-deadline-cutoff`
**Last merged exact head:** `113159871228c157883439a33422fef0405a3e9d`
(approved on review pass 13; merged as `48ea494c`)
**Last reviewed exact head:** `5ac31a5c193eee75b4f3868a7614bfeb192383c6`
(round 6 **APPROVED**)
**Current repair implementation:**
`97916906c774a26242ba417a23c446aca213772d`, with history binding
`248d675b60dbd92c55bb460f93fe7782ca7889f9`; this documentation snapshot
follows them, so resolve the immutable branch tip before requesting round 7
**Test state:** the first authorized focused command reached the Rust compiler
but ran zero tests because of three test-build errors. The errors are repaired
but not recompiled or retested; exact-head round 7 must approve first.

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
  running unit tests. If a later correction changes that head, obtain another
  exact-head approval before merge.
- Merge only after focused tests, `make check`, `make cluster-check`, and every
  required hosted check are green, with no unresolved actionable review item.
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

The merged actor is behavior-neutral for recovery. Legacy rolling watchdogs
and in-place replacement still operate until later M4 slices transfer action
authority to the actor/executor and then delete the old owners.

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

The action-passive deadline slice is in PR #621. Review round 6 **APPROVED**
exact head `5ac31a5c193eee75b4f3868a7614bfeb192383c6`. The first focused command found
three compile errors before running tests; repair commits `97916906` and
`248d675b` remain uncompiled and untested pending exact-head round 7.

- Push this candidate, resolve the immutable remote head, and request
  adversarial round-7 review of that exact commit.
- Do not run unit tests until that review approves. After approval, run focused
  deadline/ingress tests, then `make check`, `make cluster-check`, and hosted
  CI. Fix every failure and re-review any changed head before merge.

Do not skip the adversarial-review gate because an automatically started
hosted workflow happened to be green. The user explicitly ordered adversarial
review before local unit tests and full verification before merge.

## 5. Current bounded M4 slice after #619

A read-only Luna scout inspected the contract and current code without editing
or testing. Its recommended smallest slice is now in the PR candidate:

- add actor-private `ProducerProgressDeadline` state beside producer facts in
  `RollingControlActor`;
- arm `starting` in `begin_producer_attempt_at`;
- rearm `advancing` from accepted ingress `published_at`, never delayed
  `observed_at`;
- classify non-success exit immediately and enter `classifying_exit` only for
  a successful exact exit needing completion proof;
- choose the nearest lease/producer deadline in `run`, with a post-receive
  due-first cutoff rather than relying only on `tokio::select!` bias; and
- emit no retry/kill/replace action yet.

The local candidate retains the exact armed instant in a provisional
action-passive due record, applies exact-boundary progress or non-success exit
before cutoff, gives lease/session terminal state priority, and disarms the
passive deadline after one observation so it cannot spin. Producer cutoff now
captures under the synchronous transition plus ingress fence and folds once;
the sole process supervisor publishes successful exact-attempt SIGSTOP/SIGCONT
as ordered ingress barriers before releasing that fence. Full bounded ingress
waits before the syscall and re-authorizes after actor drain; intentional holds
disarm the clock and a resume grants a fresh full budget. The temporary
starting budget is the conservative 30-second compatibility value until the
next policy-admission slice supplies the existing 12-second hardware or
30-second software/copy budget. No recovery behavior consults this passive
observation.

The armed `ProducerProgressDeadline`, provisional due record, process-exit due,
flow revision, and physical-flow state remain actor-private. They are not yet
projected through `RollingLeaseSnapshot`, Activity/status, or Prometheus; only
bounded flow-capacity deferrals are exported. The decision/action slice must
add a bounded operational projection before any legacy watchdog is removed.

Primary implementation sites are in `crates/plurxd/src/playback_control.rs`:

- `ProgressCoverageBatch` and ingress near line 1892;
- `RollingControlActor` producer state near line 2411;
- `begin_producer_attempt_at` near line 2758;
- `observe_producer_progress_at` near line 2826;
- `observe_producer_exit_at` near line 2849; and
- actor `run` near line 3421.

Authored but not yet run focused evidence covers exact starting expiry and
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
lifecycle-expiry precedence, process-owner flow publication, and lifecycle commands
before/at/after the provisional producer due coordinate.
Command publication sequencing remains deliberately outside this
action-passive slice; it must land before a producer deadline is allowed to
emit a decision.

Committed implementation and review-repair sequence through the compile-fix
history binding preceding this documentation snapshot:

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
```

Leave all compatibility owners unchanged and active in that slice:
`FIRST_SEGMENT_GRACE`, `SOFTWARE_GRACE`, `PROGRESS_STALL`, `WATCHDOG_POLL`,
`watch_for_stall*`, `playlist_producer_failed`, `downgrade_one_step`,
`child_transition`, `watchdog_active`, and `replacing_child`. The passive actor
deadline observes and records first; later slices move action authority and
delete legacy owners without two concurrent recovery decision makers.

## 6. Remaining roadmap

After the passive deadline slice:

1. Add one immutable actor decision and nonblocking session-executor wake.
2. Move the single allowed pre-publication validated retry behind that owner.
3. Convert exact exits and copy classification to actor decisions.
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
