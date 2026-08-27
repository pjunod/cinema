# Playback control rewrite — implementation handoff

**Updated:** 2026-08-27
**Merged baseline:** `origin/main` at `f9cef83b` (PR #618)
**Active PR:** [#619](https://github.com/pjunod/plurx/pull/619)
**Active branch:** `codex/playback-control-m4-actor-deadline`
**Last reviewed head:** `10c95e6d4fbc7e56079bef336b7c806658ed2673`
(approved; no P1/P2/P3 findings)
**Current exact head:** resolve
`origin/codex/playback-control-m4-actor-deadline`; the approval successor is
documentation-only gate evidence and requires final exact-head review
**Test state:** focused tests, `make check`, and `make cluster-check` green;
hosted checks pending on the final evidence head

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

The merged actor is behavior-neutral for recovery. Legacy rolling watchdogs
and in-place replacement still operate until later M4 slices activate the
actor deadline/executor and then delete the old owners.

## 3. PR #619 — exact current state

### Intended slice

This PR does two prerequisite jobs without changing recovery decisions:

1. retain a constant-space, cutoff-safe chain of producer-progress evidence;
2. establish a checked baseline inventory of every old recovery owner and all
   task, timer, process-start, lifecycle, and alias shapes that could recreate
   one.

The actor still receives one behavior-neutral projection. No
`ProducerProgressDeadline` is armed and no legacy watchdog is removed in this
PR.

### Committed implementation

The pushed branch contains:

- `ProgressCoverageBatch`, retaining first advancing progress, contiguous
  covered tail/deadline, first gap, latest progress, and latest telemetry;
- a persistent exact-attempt progress watermark across ingress drains;
- rejection of repeated/regressing timestamps as telemetry-only evidence;
- successor-attempt reset with late-predecessor rejection;
- exit barriers that seal preceding progress;
- an interim legacy-owner catalog and validation test;
- synchronized Markdown and HTML status pages; and
- regression-history evidence.

Committed branch sequence through the last pushed head:

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

### Adversarial review chronology

Eleven exact-head reviews have run. No tests were run during them.

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

The reviewer has consistently confirmed the producer-ingress and successor
watermark behavior after the first corrections. The remaining work is the
static structural contract, not runtime semantics.

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

The current implementation is approved and all local gates are green. The
documentation-only evidence commit needs a final exact-head adversarial
review while hosted CI runs.

- Push the gate-evidence commit and request final exact-head review.
- Require every hosted check green. Investigate and fix any failure; any
  changed head requires another exact-head review and affected local gates.
- Record exact SHAs and test counts here and in status, merge PR #619,
  fast-forward the disposable clone to merged `main`, create the next
  `codex/` branch, and continue M4.

Do not skip the adversarial-review gate because an automatically started
hosted workflow happened to be green. The user explicitly ordered adversarial
review before local unit tests and full verification before merge.

## 5. Next bounded M4 slice after #619

A read-only Luna scout inspected the contract and current code without editing
or testing. Its recommended smallest slice is an action-passive activation of
the actor's producer deadline and due-first cutoff:

- add actor-private `ProducerProgressDeadline` state beside producer facts in
  `RollingControlActor`;
- arm `starting` in `begin_producer_attempt_at`;
- rearm `advancing` from accepted ingress `published_at`, never delayed
  `observed_at`;
- enter `classifying_exit` on exact exit;
- choose the nearest lease/producer deadline in `run`, with a post-receive
  due-first cutoff rather than relying only on `tokio::select!` bias; and
- emit no retry/kill/replace action yet.

Primary implementation sites are in `crates/plurxd/src/playback_control.rs`:

- `ProgressCoverageBatch` and ingress drain near lines 1787–1904;
- `RollingControlActor` producer state near lines 2197–2217;
- `begin_producer_attempt_at` near lines 2407–2427;
- `observe_producer_progress_at` near lines 2429–2460;
- `observe_producer_exit_at` near lines 2462–2483; and
- actor `run` near lines 2810–2861.

Focused evidence must cover exact starting expiry, rearm from fenced
publication time, late-progress cutoff, contiguous versus gapped A/B/C
coverage, command/exit barriers, classification mode, lease-terminal priority,
deadline-versus-ready-event ties, and scheduler-delayed dispatch.

Leave all compatibility owners unchanged and active in that slice:
`FIRST_SEGMENT_GRACE`, `SOFTWARE_GRACE`, `PROGRESS_STALL`, `WATCHDOG_POLL`,
`watch_for_stall*`, `playlist_producer_failed`, `downgrade_one_step`,
`child_transition`, `watchdog_active`, and `replacing_child`. The active actor
deadline observes and records first; later slices move action authority and
delete legacy owners without two concurrent recovery decision makers.

## 6. Remaining roadmap

After the passive deadline slice:

1. Add one immutable actor decision and nonblocking session-executor wake.
2. Move the single allowed pre-publication validated retry behind that owner.
3. Convert exact exits and copy classification to actor decisions.
4. Fence physical hold/resume actions and acknowledgements.
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
