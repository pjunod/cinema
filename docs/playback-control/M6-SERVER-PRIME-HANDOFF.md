# M6 phase 3 — reserve and prime

**Status:** implemented on `codex/m6-server-prime`; validation and merge pending ·
**Written:** 2026-09-08 · **Implementation update:** 2026-09-09 ·
**Implementation base:** `main` at `75744fea`

All three client halves are merged. The server implementation now reserves the
durable successor, attaches its VOD reader/producer before actor publication,
authorizes only the exact staged media route, preserves that media after commit
while the predecessor drains, and releases the worker on every abort path. This
document retains the preimplementation diagnosis and the constraints used to
build that change.

Settings → Developer exposes a direct, default-on
`prepared_quality_handoff` checkbox. Its readiness rows are advisory only; no
qualification result is consulted by the server enable path.

---

## 1. What was missing, precisely

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
`410 media_owner_lost` after `PREPARATION_DEADLINE_MS` (330 s, derived —
see §6 step 1). A client that builds a
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
lets the driver reclaim the producer. It needs wiring into the exits that
actually tear a successor down, not inventing.

> **Corrected 2026-09-08.** This paragraph used to name "all four exits —
> abort, the 330-second deadline, a refused commit, and `reject_commit`", and
> §6 step 1's note gives a *different* four. Both were written from a reading
> rather than an enumeration, and a document with two incompatible lists of
> "the four exits" is worse than one with none. The enumeration is in §6 step 1
> and it is the one to use; this paragraph now points at it instead of
> competing with it. The deadline is `PREPARATION_DEADLINE_MS`, derived rather
> than literal — see that note.

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

**Implementation result, 2026-09-09.** Commit settlement now returns the
durable outcome within the client's four-second exchange and runs publication
separately. It tries the exact predecessor terminal projection first, then
uses the existing safety-boundary handoff if the predecessor is still draining.
The committed successor remains readable during that interval only when its
stored start response carries the server's prepared marker and the exact
current playback pointer still names the same owner, epoch, session and user.
That is a media capability, not a second control authority: status, delete and
control continue to honor the publication fence.

`renew_media_sessions` and `owned_media_sessions` now use the live preparation
ledger predicate rather than the sentinel as their definition of staged. That
keeps a committed successor renewable and discoverable by the publication
reconciler while keeping an uncommitted successor out of ordinary inventory.
The SQLite and replicated queries implement the same predicate.

## 5. The decision, taken 2026-09-08 (§5.1; historical baseline)

**Current implementation correction, 2026-09-09.** The refusal described in
this section no longer exists in current `main`. Both ordinary VOD creation
and `vod_resurrect_before` resolve an encoded request through
`prepare_vod_encoding` and pass the resulting `Encoding` into `VodServe`.
Therefore the prepared-successor path primes supported transcoded recipes as
well as copy recipes. The copy-to-copy transition remains the clean transaction
control, but it is not the only recipe the shipped engine can attach. The
numbered options below record the decision made against the older baseline;
they are not an enable gate in the current code.

**The transition the FLEET produces makes a recipe the server cannot serve.**

> **Correction, 2026-09-08.** This line read *"The only transition M6 admits
> produces a recipe the server cannot serve"*, and that is false. See
> §5.1 — there is an admitted, receipted, copy-only transition the VOD engine
> serves today, and it is what phase 3 should be built and proven against.

- `PREPARED_AXIS_SETS` admits `{ResolutionOrBitrate}` and
  `{ResolutionOrBitrate, DeliveryMethod}`. Shadow mode established that a pure
  resolution change does not occur on a real library — *"the top rung
  direct-plays and the lower rungs transcode, so the delivery method moves with
  the height every time"*. The live case is copy → transcoded rung.
- `candidate_request` therefore produces `SessionKind::Transcode { height }`.
- At the document's original baseline,
  `VodServe::try_create_with_release_fence` refused every non-`Copy` kind:
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

### 5.1 Answered 2026-09-08, and the first answer was wrong

**Build phase 3 now, and prove it on the copy-to-copy transition that is
already admitted.** Do not narrow `PREPARED_AXIS_SETS`, and do not wait for D6.

An earlier revision of this section answered "hold for D6" on the reasoning
that no transition M6 admits can be served today. An adversarial review checked
that premise against the code instead of the prose and it does not survive.

**There is an admitted, receipted, copy-only transition, and the engine serves
it.** `candidate_request` has three arms that leave a `Copy` predecessor a
`Copy`: `QualitySelection::Auto`, `QualitySelection::Original`, and
`Manual { height }` where the height is the source height — *"Auto and Original
leave a copy copying, and a source-height ask is a copy's own delivery asked
for by name."* `EffectiveSelection::from_request` maps `SessionKind::Copy` to
codec `"source"`, so both sides read `source` and **no `DeliveryMethod`
crossing occurs**. The crossing test fires `ResolutionOrBitrate` when the
height *or* `quality_auto` differs. So a direct-playing session moving between
Auto and Original — or Auto and a pinned source rung — crosses
`{ResolutionOrBitrate}` **alone**, which:

- is in `PREPARED_AXIS_SETS`;
- has a receipt, M5.5 on Apple, 2026-09-01, 20/20;
- passes `successor_rate_is_bounded`, which only constrains the pair;
- and yields a `SessionKind::Copy` candidate, so
  `VodServe::try_create_with_release_fence` **serves it** — the
  `vod_transcode_unavailable` refusal never fires.

The module's own test asserts exactly this returns
`Prepare { ResolutionOrBitrate }`. It is a transition a viewer makes: toggling
Auto on a title that direct-plays.

**So phase 3 was buildable against a real production transition, with no new
hardware receipt and no change to the axis table.** At that baseline the
transcode case still met the D6 refusal described above. The 2026-09-09
correction at the start of this section records why that statement no longer
describes current `main`.

**What the earlier revision got wrong, recorded because the shape recurs.** It
enumerated the copy-only transitions as *"`{AudioTrackOrOffset}` and a subtitle
burn removal"*, and reasoned that since neither axis has a receipt, narrowing
was a hardware dependency in disguise. Both halves were wrong. Option 2 four
paragraphs above says *"audio track, source-height **Original**"* — the item
silently dropped is the one carrying the receipt. And a burn removal is
unreachable anyway: `try_create_with_release_fence` refuses
`req.subtitle_burn.is_some()` with `vod_subtitle_burn_unavailable`
independently of D6, so a VOD session with a burn cannot exist to transition
out of, and a burn removal moves the delivery method too, making it a pair
rather than an axis. Substituting an example for the one in the list is how a
false premise reads as true.

**What the copy-to-copy case proves, and what it does not.** For a `Copy`,
`candidate_request` copies every field and changes only `automatic`: the height
is the source height on both sides, and `automatic` is consulted for a `Copy`
in exactly one place, the session fingerprint. So predecessor and successor are
two distinct sessions serving **byte-identical media**. That is the ideal
control for a transaction test — nothing about the handoff can be explained by
the media changing — but it means this case proves *the transaction*, not a
changed pipeline. Say so when it lands rather than letting a reader infer a
media handoff was proven. The first thing to confirm in the build is that two
VOD sessions on the same file and playback id can coexist; the differing
`automatic` gives them distinct fingerprints, so the expectation is yes, and
this document asserts it implicitly.

**Current result, 2026-09-09.** D6 is no longer a code gate on VOD transcode
creation. A copy dropping to a supported transcoded rung resolves and attaches
through the same VOD encoding path as an ordinary session. Device measurements
remain valuable acceptance evidence, but their presence is not consulted by
prepared-handoff enablement or worker creation.

The 2026-09-09 implementation followed §6's ordering and retains the
copy-to-copy case as the transaction control. Transcoded candidates also use
the real VOD attach path and therefore inherit that engine's own admission and
availability decisions; this change adds no qualification gate of its own.

## 6. The order to build it in

**Implementation result, 2026-09-09.** The production path now performs the
following sequence:

1. reserve the durable successor row and its preparation ledger;
2. attach the VOD reader/producer with the successor's exact recipe, identity,
   adoption token and deadline;
3. announce the prepared action only after attachment succeeds;
4. authorize pre-commit media only through the exact live ledger capability;
5. after commit, require the durable prepared marker and exact current pointer
   for media during predecessor drain, while control/status stay fenced;
6. keep the VOD timeline origin at zero and carry the accepted playhead only as
   the recipe/bootstrap start, so recovery never adds the resume twice;
7. renew and inventory the committed successor, then publish its control plane
   after predecessor projection or the existing safety boundary; and
8. release the worker and terminally settle the durable row on abort, refusal,
   expiry or failed attachment.

The live-process deadline task uses the remaining durable deadline and releases
the actual worker. The durable preparation deadline and ordinary process
restart semantics remain the crash backstop: a restarted daemon has no old
in-memory VOD worker to leak, while store maintenance still aborts the expired
row. No readiness result participates in the enable decision.

1. Free the worker on every exit first — `begin_end_detached` in
   `PreparationExecutor::abort`, in the deadline task, and on every exit that
   discards a successor — and move the deadline into the actor's
   `next_deadline()` per `M5.5-STAGED-GENERATIONS-HANDOFF.md` §6.1. A detached
   `tokio::spawn(sleep)` loses its timer on restart, which with a worker
   attached leaks an ffmpeg until the idle sweep. **Build the teardown before
   the thing that needs tearing down.**

   > **Scoped 2026-09-08, and two things this step said are not true today.**
   >
   > **It is pre-work, not a live leak fix.** `stage_prepared_successor` writes
   > a durable row and takes the slot; it creates no rendition, no driver and
   > no ffmpeg. There is nothing to leak until step 3 exists. That is what
   > "build the teardown first" means, and it is worth saying plainly because
   > the sentence reads as a present-tense bug.
   >
   > **"Both refusal branches" undercounts, and one of them is not a
   > teardown at all.** Three exits discard a successor: stage refused by the
   > slot (the durable row is rolled back), commit refused by the store's CAS,
   > and `reject_commit`. `abort()` is the fourth, named separately in the step
   > text above.
   >
   > The commit-gate refusal is a fifth *exit* and deliberately not a teardown:
   > `may_commit_preparation_for_owner` is a pure `&self` read, and on refusal
   > `commit` returns `Refused` having touched neither the store nor the slot.
   > It fires exactly when the slot no longer holds this successor — the
   > deadline already aborted it — or the epoch is stale, so wiring
   > `begin_end_detached` in there would be a candidate double-free rather
   > than a plugged hole. **Leave it alone**, and do not read the earlier
   > wording as having missed it.
   >
   > **The constant is `PREPARATION_DEADLINE_MS` in `http/hls.rs`**, derived as
   > `VOD_LEASE_TIMEOUT_MS + 30_000` — there is no literal `330` to grep for,
   > and the derivation is load-bearing.
   >
   > **The obstacle to name before starting.** `next_deadline()` returns a
   > monotonic `Instant`; `PreparationSlot::Staged.deadline_ms` is absolute
   > unix ms, so the slot needs a companion `Instant` recorded at stage time
   > before it can be folded in with a `.min(...)`. And the firing arm gives
   > you a synchronous `&mut self` in the actor, while
   > `PreparationExecutor::abort` is `async` and makes a store round trip —
   > the detached spawn gets that for free and the actor does not. It needs the
   > directive shape `PreparationDirective` already uses, or the actor blocks.
   > That routing is a design decision rather than a transcription, and it is
   > the part that can blow any estimate of this step. **A scoping pass on
   > 2026-09-08 put the rest at roughly 120-180 lines** across the constant's
   > removal, a companion `Instant` on the slot and its construction and match
   > sites, the `.min(...)`, a new `settle_preparation_deadline_at`, and its
   > two call sites. That figure is one afternoon's reading and nothing in this
   > repository corroborates it — treat it as an order of magnitude, not a
   > budget, and re-derive it before planning around it.
   >
   > **Add the settle to both entry points.** `settle_due_deadlines_at` is
   > `#[cfg(test)]`; production fires through `handle_producer_blocks_at`. A
   > settle added only to the first passes every test and does nothing in
   > production.
   >
   > **Follow these tests:** `the_next_wake_accounts_for_an_outstanding_action`
   > for the fold, `lease_terminal_wins_an_exact_tie_with_the_producer_deadline`
   > for §6.1's priority rule, and
   > `actor_timer_publishes_the_fence_at_each_modes_exact_deadline` for proving
   > the wake actually fires under `tokio::time` pause/advance.
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
