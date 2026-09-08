# M6 phase 3 — reserve and prime, and the decision it waits on

**Status:** open · **Blocks:** every client half of M6 doing anything a viewer
sees · **Written:** 2026-09-08 · **Baseline:** `main` at `3006f38a`

The client halves of M6 are being built now — Apple is merged, web and Android
are in flight. None of them can fire, because the server stages a successor and
never produces media for it. This document is what the eight-phase plan's third
phase actually requires, what it must not break, and the one decision that has
to be made before it can be built.

It is deliberately not a plan you can execute end to end today. **Read §5
first**: the only transition M6 currently admits produces a recipe the VOD
engine refuses outright, and no amount of plumbing removes that.

---

## 1. What is missing, precisely

`docs/playback-control/PLAYBACK-CONTROL-PROTOCOL-PLAN.md` §5.1 names eight
phases — propose · stage · **reserve and prime** · prepare · commit · commit
durability · retire · abort. Phase 3 is the only one with no implementation,
and `stage_prepared_successor`'s own doc comment says so: *"**Stage only.** …
a durable row and the actor's one successor slot, nothing produced yet."*

The consequence is not that the handoff is slower. It is that it cannot
happen at all:

- The staged row is written at `MEDIA_SESSION_PUBLICATION_BLOCKED`
  (`plurx-core/src/domain.rs`).
- `classify_durable_route` (`plurxd/src/media_sessions.rs`) answers
  `OwnerTransition` for any row whose fence is not `0` — and it tests the
  fence *before* the owner-node test.
- `relay_if_remote` (`plurxd/src/http/hls.rs`) refuses on that classification
  before any playlist work runs.

So **a `GET` of the successor's `playlist_url` answers `503
media_owner_transition` on every request** while the staged lease is live, and
`410 media_owner_lost` after the 330-second deadline. A client that builds a
second pipeline on that URL waits out its readiness bound and gives up, every
time.

Phase 3's requirement, verbatim from §5.1:

> **3. Reserve and prime.** Admission is requested without releasing the
> current permit unless a single-slot handoff uses §5.3's weaker contract. The
> successor uses an idempotent `request_id` derived from `action_id`, begins at
> an aligned film-time boundary, and publishes enough media for the target
> platform's measured startup need.

Nothing anywhere names a function, a session id, an attachment or a
publication step. There is no competing design to execute — there is a
paragraph and a set of constraints.

## 2. What already exists, and is the right body

**Production is per-rendition, not per-session.** A rendition is keyed by
recipe and source identity; sessions are readers on it. Crucially, *a bare
reader already creates production*: `playback_demands` appends an idle demand
for every reader with no blocked `GET`, and the producer scheduler turns that
into ahead-fill up to the ahead horizon. **Attaching a session whose reader
sits at the switch boundary is, by itself, priming.** No new production
mechanism has to be written.

`Transcode::vod_resurrect_before` is the only function in the tree that takes
`(recipe_json, session_id, user_id)` and ends with a live attachment. It takes
a caller-supplied session id, is cancellation-safe, is release-fenced and
carries an admission token. It is the right *body* for phase 3 and the wrong
*trigger*: it is demand-driven, and priming has to be push-driven at stage
time, because nobody ever fetches the staged URL before it is servable. That
is the deadlock.

**Teardown is already solved.** `VodServe::begin_end_detached` writes the
tombstone and hands off to the terminal cleanup that detaches the reader and
lets the driver reclaim the producer. It needs wiring into all four exits —
abort, the 330-second deadline, a refused commit, and `reject_commit` — not
inventing.

## 3. What the fence is really for, and the one thing that must not change

`publication_ready_at_ms` is read by a dozen sites, and they do not all want
the same thing. Three genuinely need it; the rest use it as a cheap proxy for
"is this the current session", which the *pointer* already answers.

| reader | what it wants | proxy or protection |
|---|---|---|
| `classify_durable_route` | a successor must not serve while the predecessor's admitted responses are still alive | **protection** — and the site any prime design has to change or route around |
| `control_owner_refusal` | the successor must not become a second control authority before commit | **protection**, and it stays true pre-commit |
| `reconcile_owned_publication_fences` | never arm-and-publish a row the pointer does not name | **protection**, and the concrete hazard to design against |
| `owned_media_sessions`, `expired_media_sessions`, `seed_owned_lease`, `preparation_route_matches`, `staged_successor_action` | "this row is staged" | **proxy.** The real test is the ledger row in `media_session_preparations`, which is what `renew_media_sessions` already uses |

The rule the whole store half is built on is
`M5.5-STAGED-GENERATIONS-HANDOFF.md` §9: *"Do not let a staged row acquire a
pointer before commit. Every trap in §3 is downstream of that one rule."*
Nothing in phase 3 may weaken it.

**The narrow move**, if this is built: a staged-route branch in
`classify_durable_route` that resolves `ActiveLocal` **for playlist and
segment resources only** when the row is named by `media_session_preparations`,
is owned locally, and is inside its deadline — while `control_owner_refusal`
keeps refusing control on it. That preserves the fence's real job and lets
bytes flow. It is also the riskiest edit in this document: that classification
is consumed by `relay_if_remote`, `vod_resurrected_before`,
`status_local_before`, the internal relay and `verify_authority`, and
`control_owner_answer`'s own doc says there are three gates that must not
diverge. Adding a fourth shape of route is exactly how they diverge.

## 4. The second missing caller, and why it is not fixable on its own

**A committed successor is never published, and publishing it is not a
separate change.**

`commit_media_session_preparation` advances the pointer and deliberately does
not touch the fence — correct, and the same split an activation makes. An
activation then has `settle_activation_predecessor` to finish the job. A commit
has no equivalent, so the successor stays at
`MEDIA_SESSION_PUBLICATION_BLOCKED` and **is refused on both planes from its
very first request**: `classify_durable_route` answers `OwnerTransition` for any
non-zero fence, and `control_owner_refusal` refuses control on the same
predicate. The pointer moves and the viewer gets nothing — not a stream that
degrades, a stream that never starts.

**The obvious fix is a regression, and this was established by writing it.**
The publication was implemented as a caller on the commit path, the full daemon
suite passed, and an adversarial pass found what the suite could not: a
committed successor has no local worker, because nothing primes one. Moving the
row off the sentinel puts it into `owned_media_sessions` — and the lease loop
renews only sessions that are *live*. `take_stale_settlement_candidates` selects
exactly the inventory rows that are **not** live and still have runway, and
`end_media_session_if_owner` ends them `replaced` **and deletes the playback
pointer in the same transaction**. Publishing a workerless successor trades a
stalled pointer for a deleted one, seconds after the commit instead of minutes.
The sentinel was the only thing keeping the row out of that sweep.

Two further requirements the same review established, and neither is optional:

- **`PredecessorAcknowledged` may not be asserted on a retired row alone.**
  Every other use of that proof in the tree establishes the predecessor's
  terminal state first — `settle_activation_predecessor` publishes with it only
  after `project_activation_predecessor_until` confirms the row is `ended`, the
  local worker is stopped and the terminal projection is complete; otherwise it
  arms and waits the full 372-second safety window. The commit transaction
  retires the predecessor's *row* while its worker is still serving admitted
  bodies, which is durable-state equivalence and not the physical equivalence
  the proof claims. Phase 3 must either do the projection or arm and wait.
- **Bound the Store round trips.** The commit settles inside the client's
  four-second exchange deadline. `complete_activation_handoff_until` caps each
  attempt and sleeps between them; a publication that retries three times with
  no timeout and no backoff can turn a commit that durably succeeded into a
  `503` the client reads as a failed commit, and triples Store load exactly
  when the Store is already failing.

So the publication is phase 3's, not a change that can land ahead of it. What
is in the tree now is the finding, written where the next person reads it:
`preparation_executor_commits_a_staged_successor` keeps its sentinel assertion
and carries the whole mechanism in its comment, including the instruction not
to "fix" it by publishing.

**One change here is genuinely independent**, and is worth doing whenever
someone is next in this code: `owned_media_sessions` should exclude rows that
are *staged*, by the ledger predicate `renew_media_sessions` already uses,
rather than rows that are *at the sentinel*. Today those two predicates
disagree, which is why `renew_media_sessions`'s own comment — *"a committed
successor renews normally from its first tick after the commit"* — is
aspirational rather than true. It needs mirroring in the replicated backend,
and on its own it changes nothing observable, because a committed successor
without a worker is reaped by the sweep either way.

## 5. The decision, and it is not ours

**The only transition M6 admits produces a recipe the server cannot serve.**

- `PREPARED_AXIS_SETS` admits `{ResolutionOrBitrate}` and
  `{ResolutionOrBitrate, DeliveryMethod}`. Shadow mode established that a pure
  resolution change does not occur on a real library — *"the top rung
  direct-plays and the lower rungs transcode, so the delivery method moves with
  the height every time"*. The live case is copy → transcoded rung.
- `candidate_request` therefore produces `SessionKind::Transcode { height }`.
- `VodServe::try_create_with_release_fence` refuses every non-`Copy` kind:
  `vod_transcode_unavailable`, *"transcode serving is gated on the D6 device
  measurement"*. `docs/streaming/VOD-STALL-ACCEPTANCE-HANDOFF.md` confirms D6
  is open and says in as many words: do not remove the refusal, do not fake the
  measurement.

So priming cannot be built against the transition the fleet actually produces.
The options, ranked:

1. **Ship the commit-publication fix and treat priming as blocked on D6.**
   Done above for the fix; the rest waits. Honest, and it leaves the system
   correct rather than half-built. The clients already behave well under it:
   Apple's `PreparedReplacementCoordinator.canOfferPreparation` learns after
   one failed staging and stops asking, so the cost is one preparation per
   playback rather than one per quality change, and it disappears by itself the
   day priming lands.
2. **Narrow `PREPARED_AXIS_SETS` to copy-only transitions** — audio track,
   source-height "Original" — and prime those. This exercises the whole
   eight-phase transaction end to end on transitions the engine can serve
   today. The cost is that it disables the feature for the transition the
   fleet actually produces, and it contradicts the 2026-09-03 hardware receipt
   obtained specifically to widen the axis set.
3. **Finish D6 first**, then prime on a VOD engine that can serve transcoded
   rungs. Correct, and what the VOD stall handoff asks for, but it is a
   different milestone with a hardware dependency and it blocks M6 entirely.
4. **Prime through the rolling engine instead of VOD.** Rejected on sight:
   `try_vod_session` refuses takeovers into it, live-HLS recovery is a retired
   presentation behind a feature flag, and `PreparationGate`'s own doc calls
   building on the rolling registry *"the exact failure"* the gate exists to
   prevent.

**The question, in one sentence:** do we hold priming until D6 lands (1, then
3), or narrow the admitted axis set so the transaction can be built and proven
against copy-only recipes (2)?

Everything else in this document follows from that answer, which is why nothing
below §4 has been built.

## 6. If the answer is "build it", the order

1. Free the worker on every exit first — `begin_end_detached` in
   `PreparationExecutor::abort`, in the deadline task, and in both refusal
   branches — and move the 330-second deadline into the actor's
   `next_deadline()` per `M5.5-STAGED-GENERATIONS-HANDOFF.md` §6.1. A detached
   `tokio::spawn(sleep)` loses its timer on restart, which with a worker
   attached leaks an ffmpeg until the idle sweep. **Build the teardown before
   the thing that needs tearing down.**
2. The `classify_durable_route` staged-route branch (§3), with the control
   refusal explicitly unchanged, and a test per consumer of that
   classification.
3. Prime at stage time: a thin sibling of `vod_resurrect_before` called with
   the staged row's own `session_id` and `recipe_json`, reader positioned at
   `resume_ms`. **The successor must be in `renewable_session_ids()` before
   anything publishes it** — that is what keeps it out of the stale-settlement
   sweep, and it is the precondition §4 says the publication cannot be built
   without.
4. Publish at commit, bounded and proven: the predecessor's terminal
   projection first, `PredecessorAcknowledged` only after it, the safety-window
   fallback otherwise, and every Store attempt under a deadline the client's
   four-second exchange budget can absorb.
5. Account for it. A primed successor is a second rendition, a second driver
   and a second ffmpeg, competing for `HEAD_REGENERATION_CAPACITY` and the
   node's working set against real viewers — §5.1 says *"admission is
   requested without releasing the current permit"* and that request does not
   exist. `M6-CALLER-HANDOFF.md` §3.5 already costs this out.
6. Only then widen anything.

Note that **every** in-tree test of the staged path installs a session helper
that explicitly *"bypasses materialization and the producer driver"*. Nothing
in the tree has ever exercised production for a staged session, so step 3 needs
its own harness before it needs its own tests.
