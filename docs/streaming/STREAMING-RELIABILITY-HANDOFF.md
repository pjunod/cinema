# Streaming reliability — remaining-work handoff

**Status:** implementation active; not release-qualified · **Updated:**
2026-09-04, 21:25 EDT · **Owner:** the next streaming-effort agent

Companion to [STREAMING-RELIABILITY-STATUS.md](STREAMING-RELIABILITY-STATUS.md)
(progress and completed evidence),
[STREAMING-RELIABILITY-REVIEW.md](STREAMING-RELIABILITY-REVIEW.md)
(ranked findings and acceptance), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (mandatory gates).
This file contains the work still to do. Remove an item when its correction
is integrated and proved; keep completed history on the status page instead.

## Objective — finish the playback system, not just the protocol

The user wants reliable, smooth playback and transparent seek/quality/track
changes, with architectural changes as large as necessary. The last-week
review and its adversarial corrections are documented; implementation is not
complete. No production client yet runs a prepared standby transaction, and
the server's staged successor is still metadata rather than an encoding worker.

Continue autonomously with reasonable recorded decisions. Each repair PR
needs independent adversarial review, incorporated findings, focused local
proof and the blocking effort gate. Run the complete release qualification
only after the final fixes and current-main integration are frozen; fix unit
failures until green, inspect the qualification receipt, then merge to main.
Do not add hidden or compile-time feature gates. Operational prerequisites
belong in the requested Developer Enable section.

## Resume — exact authority and working copies

- **Forgejo is the sole write, PR, CI and merge authority:**
  `ssh://git@10.42.4.7:222/noirr/plurx.git`, web
  `http://forge.lan:3000/noirr/plurx`. GitHub is historical/read-only.
- **Effort:** `effort/streaming-reliability`, last verified head
  `6efec8d9b1b4a27e36a6e0a3c5af8aa773d73909`.
- **Integrated main:** `15f88e53e65362c803692a73448ee0fa7cdc1a8e`.
  Refresh both refs before resuming; they may advance independently.
- **Never use the user's checkouts for implementation.** The paths below
  are agent-owned worktrees of an agent-owned clone. Preserve dirty work.
- **SSH key:** `~/code/plurx-agent/.ssh-deploy-key`.
  Use it through SSH; do not print, copy into containers, or commit it.

| Work | Agent-owned path | Branch / checkpoint |
|---|---|---|
| This handoff; no runtime edits | `/private/tmp/plurx-desired-intent.CT4s5M/repo` | `codex/streaming-reliability-handoff`; base `6efec8d9`; pinned local compiler loop available; exact-base hook required before publication |
| Integrated capture reference | `/private/tmp/plurx-control-capture.AazoEB/repo` | `codex/control-capture-envelope`; clean, published `8a7829b512ec0151bb7d2477e86898a1f0f26eb5`; PR #20 merged as `6efec8d9` |
| VOD production — **dirty work must be preserved** | `/private/tmp/plurx-streaming-vod-recipes.nE72nh/repo` | `codex/vod-recipe-production`; committed base `288992e0`, with substantial uncommitted transcode/burn production and tests |
| Common clone and warm Rust target | `/private/tmp/plurx-streaming-reliability.1WSd2I/repo` | Do not change its old task branch unnecessarily; its `.git` owns the worktrees |
| Approved foundation integration / reference | `/private/tmp/plurx-settlement-integration.5mawfG/repo` | clean `2013986946d72bdf7f0f2b3e57523efd2fec09be`, merged by PR #19 |
| Serving-proof proposal — **no patch applied** | `/private/tmp/plurx-serving-proof.GBqHZu/repo` | `codex/serving-proof-continuity`, clean old base `56e4f6eb`; explicit approval boundary below |

The six integrated repair/foundation PRs are #10, #14, #6, #16, #19 and #20.
They are not main promotion. Historical review/evidence PRs are already
recorded on the status page; do not redo them.

## Immediate queue — preserve proof before adding changes

1. **Finish the dirty VOD recipe task, not a second implementation.** Read
   that worktree's `docs/streaming/VOD-ENCODING.md` and `docs/streaming/VOD-M3-HANDOFF.md`, inspect
   its diff/untracked files, and obtain the final source manifest and review.
   It is not approved for commit/push merely because intermediate tests pass.
   Active files include `vodencode.rs`, `vodencode_tests.rs`,
   `vodencode_manager_tests.rs`, `vodgen.rs`, `vodserve.rs`, `prodrun.rs`,
   `admission.rs`, `ffmpeg.rs`, `subtitles.rs`, the narrow HLS error mapper,
   `plurx-core/src/transcode/vod.rs`, and ownership inventory/documentation.
   The latest review also found that the burn-subtitle extractor's
   `.output()` has a timeout but no buffered stderr byte bound. Fix the
   diagnostic-memory ownership and prove it before final approval. Current
   final tested-source manifest is `00988814a677192357753f5d128f6b7558525d45e116e8da8c6543232a611a2d`;
   adding the author's checkpoint changed documentation-only manifest to
   `b9734eb4dcde93398f0d9c490cccc1581dd4763f97fa44ff715a98293981b99d`.
   Recompute after fixes and current-effort integration; neither is approval.
   The VOD author is now implementing that bounded diagnostic/reap correction;
   the prior frozen manifest is only a checkpoint, not current-source proof.
   A second final-review issue is `driver_pass` awaiting capacity policy while
   holding `rendition.manifest`: a slow settings read then blocks already-cached
   segment pin/open/publication. Move policy I/O outside that lock, revalidate
   the exact epoch/demand before acting, and prove cached GET responsiveness
   and single-permit restart safety through the real driver boundary.

## Remaining implementation — in dependency order

This grouping does not waive any accepted finding in the ranked review.
Reconcile its still-open rows against final implementation/evidence before
calling the effort complete, including smaller validation/operations defects.

### 1. Accepted film time and durable desired intent

**Landed — do not implement again.** The correction below shipped as `ee4dfbe3`
(PR #24, merged into `effort/streaming-reliability` at `6f09ffa5`).
`accepted_film_time_ms` is computed at the control-to-staging seam, carried on
`PreparationCandidateInputs`, and consumed as `resume_ms` without re-adding the
route origin; the doc comment there records the old shape so it cannot come
back. The paragraph is kept as written because the closing sentence is still
true: it did **not** close stale-intent ordering or physical cutover, which is
what the durable fix below is for.

**Independent small correction ready to implement:**
`http/hls.rs::stage_prepared_successor` currently computes resume from
`route.media_origin_ms + route.fetched_through_ms`. This skips buffered but
unseen film. Capture `seek_target_ms.unwrap_or(position_ms)` from the accepted
control envelope, carry it through `PreparationCandidateInputs`, and persist
that absolute film time without adding route origin again. Prove playhead
30 seconds / fetched 150 seconds, nonzero origin, and backward seek at the
production control-to-staging boundary. Replace the obsolete frontier test
and its historical regression rationale; do not bypass the history hook.
This correction does **not** close stale-intent ordering or physical cutover.

**The durable fix is essential, not optional polish:**

- Use a stable per-title/controller lifetime, separately monotonic recipe,
  destination and transport revisions, and a normalized complete selection.
  Preserve Auto, Original and Manual as different requested policies.
- Capture-local `sourceRevision` changes on cadence; attachment generation
  identifies a reporter; generic intent generation mixes recovery/transport.
  None is the durable media-intent revision. Add an explicit shared envelope
  to ordinary create **and** control, including when advisory control is off.
- Persist canonical desired ownership before reporting an intent-changing
  request accepted. Retain cancellation-independent outcome/replay ownership.
- Atomically compare expected desired revision/digest in preparation admission,
  prepared commit **and ordinary activation**, alongside existing predecessor,
  owner, deadline and identity predicates. A pre-await check cannot close the
  Store-await gap. If C wins first, B must fail; if B commits first, C must
  reconcile against B instead of being discarded.
- Local desired state must update within successful acceptance **before**
  old Prepare/Commit acknowledgement selection. `accept` currently precedes
  `observe`; extending only `observe` is too late for C plus B's old commit
  acknowledgement in one request. Rejected/replayed packets cannot advance it.
- Retain/coalesce the latest desired work and wake it after obsolete slot
  cleanup. Spawning once on `selection.changed` loses C if B occupies the slot.
- Heartbeats, advancing playhead and buffer samples do not supersede media
  intent. Pause changes transport without discarding a valid recipe. End and
  title/owner lifetime changes fence work and transfer exact cleanup ownership.
  Today's wire cannot distinguish coalesced/repeated same-target seeks with
  no observed completion; do not infer command identity from clock movement.

Store checkpoint at `12be7b5b`: SQLite schema **47**, replicated AUTH schema
**27**. The next available versions were 48/28; verify again before editing.
Update shared schema SQL, SQLite migration list, replicated migration target,
`schema_migration_action`, bootstrap/settlement paths, import TablePlan and
minimum-schema projections, parity/empty-target checks, and dump/digests.
**A schema bump alone does not fence already-running old writers:** compatibility
is checked at open/preflight, not every transaction. Once canonical ownership
exists, DB constraints/triggers must reject old missing/stale target tokens
at pointer changes/preparation admission, or old writers must be verifiably
drained. Preserve predecessor renewal/read authority during preparation.
Current import orders pointers/preparations before media sessions; new
reference-checking triggers require deliberate restore ordering/reconciliation.
Legacy missing tokens are permissible only where no canonical owner/tombstone
exists. Never replace a missing expected token with the current revision or
delete ownership so an old writer can regain authority. Audit replay, rejoin,
takeover and direct pointer mutation as well as the named new APIs. Preserve
desired/replay/tombstone rows on import and legitimate older-token predecessor
rows. Existing AUTH_PROTOCOL 4–5 concerns Raft/learner compatibility, not this
client wire contract; do not bump it blindly. Both SQLite and three-voter Store
proof must cover old SQL on an already-open upgraded database, commit-unknown,
restart and import—not only an old binary refusing to reopen the database.

Required races: held B source/height read then C; same-quality seek; ABA;
unchanged heartbeat; C plus B acknowledgement; occupied-slot coalescing;
End/rollover/replay/rejected packet; held Store commit with both orderings;
overlapping control-disabled creates; old-writer SQL and migration/import.

### 2. Real immutable VOD media and responsive long-film seeks

The dirty VOD task already exercises actual create → GET → file → decode,
NTSC 0 → 90.09 → 3-second restarts, VFR normalization, text/PGS burn spanning
a seek, ±250 ms audio correction, and independent adjacent AAC decode joins.
Preserve those production-linked tests and their source/initialization identity.

Remaining review seams include full strong source/sidecar/executable identity,
current (not creation-time) capacity policy, typed HTTP recipe refusals and
bounded policy reads. The latest correction uses one settings-pair read with
a one-second deadline; verify its held-read regression and no leaked demand.
HDR fixtures must contain genuinely PQ-domain values, not only color labels.
FFmpeg 9 frame metadata can override codec CLI color options. Hardware-display
quality is not established by software Main10/PQ decode tests.

**Performance remains open:** decoded audio currently starts at film origin
to preserve exact global sample/AAC phase. It fixes an observed eight-sample
join error, but a serial warm local two-hour 5.1 fixture takes **7.629 seconds**
near 7195.188 seconds, with 12.189 seconds child CPU. A separate earlier
loaded run took 12.273 seconds. These meet a materialization bound, not
responsive/seamless acceptance; cold NAS may be worse. The serial log is
`/private/tmp/plurx-astra-vod-recipes-final-two-hour.log`. Build a seekable canonical audio artifact
or exact-sample seek index with bounded shared ownership, source identity and
cache accounting; repeat independently decoded forward/reverse joins and
serial long-film/cold-storage benchmarks. Do not trade continuity for speed.

### 3. Truthful delivery, failure, capacity and recovery

**Audited against the code 2026-09-06. Verdicts are recorded per bullet with
the evidence, because three of these had already been done and were still
being read as open — which is how two sessions came to investigate the same
fixture defect on the same day.**

- **DONE.** ~~`DeliveryView::from_status` still drops VOD producer failure and
  delivered bitrate.~~ The VOD arm carries `delivered_bps`, `delivered_idle_ms`
  and `producer_decision` (`playback_control.rs:933-967`), measured over
  monotonic time by `meter.rs` and never substituted from the encoder's
  configured target. Reasons are bounded on the wire (`:698-704`), parsed from
  a closed set, and a relayed verdict that disagrees with delivery is refused
  (`:730-747`).
- **DONE.** Preparation reaches VOD: `headroom_refusal` has the delivered rate
  it needed (`playback_control.rs:1280-1293`, regression at `:13504`).
  Contiguity is now read rather than discarded — `contiguous_runway_ms` credits
  runway only when the client's reported region reaches the playhead, and an
  absent `buffered_from_ms` keeps the old reading because a missing field is no
  evidence rather than a hole
  (`runway_counts_only_a_buffer_that_reaches_the_playhead`).

  Transport versus displayed progress needed no change, and that is worth
  stating rather than leaving as a suspicion. The two facts arrive from two
  sources — the server's own `fetched_through_ms` and the client's
  `position_ms`/`render_state` — and no consumer substitutes one for the other.
  Both places that resume from a frontier pull it back by a whole segment
  precisely *because* fetched runs ahead of seen (`media_sessions.rs:2626`
  `owner_loss_resume`, `:4902` `takeover_resume`, with the reasoning at
  `:2555-2565` and `:4898-4901`), and `recovery_outranks_hold` takes the
  request and the delivery view separately and reads each for what it is. A
  rendered-progress field on `DeliveryView` would duplicate what the request
  already carries.
- **PARTIAL — fairness is the only piece left, and it is deferred by decision.** Session-close cleanup is
  done (`waitpool.rs` `retire_session`, wired at `vodserve.rs:2825`), and both
  caps with typed refusal classes are done. Still open, and each for a stated
  reason:
  - *Fair service at the global limit.* **DONE, as a reservation.** A session
    holding no waits may draw on the whole node cap; a session that already
    has a foothold stops at `general_admission` — an eighth of the cap, at
    least one slot, is reachable only by a viewer holding nothing.

    The old behaviour was pure arrival order below the per-session ceiling,
    which is worse than it sounds: a struggling client retries, so the viewers
    doing worst competed hardest for slots and crowded out the ones who would
    have been fine. A viewer forty minutes into a film lost to whoever asked
    first.

    A reservation rather than round-robin, by decision. Round-robin is fairer
    in principle and needs ordering, per-waiter bookkeeping and a starvation
    rule; the pathology worth fixing is narrower — a session with slots taking
    the last slot from a session with none — and one comparison fixes exactly
    that with no state that can go stale. The test that used to pin the gap by
    name now pins the fix, and removing the reservation fails it.
  - *Total GET admission.* **Visible, still uncapped.** The pool admits
    **blocked** GETs only; a GET served from materialized bytes is admitted
    against nothing, and until now was also *counted* by nothing — a node
    serving a hundred concurrent cache hits and one serving none reported the
    same numbers. `BlockedGetMetrics::serving` and `plurx_vod_gets_serving`
    now count them, held by a guard so the gauge falls on client disconnect
    rather than only on success
    (`a_served_get_is_counted_and_bounded_by_nothing`).

    **Decided: no cap, and this is the record of that.** Refusing a viewer a
    segment already on disk is a choice about what that viewer loses, and a
    cache hit is the cheapest thing the server does — a read of bytes it
    already holds. The gauge runs first; if the number ever justifies a
    ceiling, bound *bytes in flight* rather than request count, because that is
    the resource actually contended. Whoever adds one fails
    `a_full_cache_admits_another_rendition` and must say in its replacement
    what happens to the viewer who hits it.
  - *Bounded `NoRoom`.* **DONE.** The decision was taken as a bound on `Hold`
    itself rather than a new action or a capacity variant of
    `ProducerDecisionReason` — node capacity is not a producer decision, and
    that category error is why this was blocked. `Hold` now carries
    `revisit_after_ms`, and `no_room` asks for four ordinary exchanges rather
    than one because nothing the client does clears it.

    It is a revisit contract, not an expiry: nothing fails when the interval
    passes. Escalating to a terminal was rejected for the reason recorded here
    — it would tell a client to tear down a player over a server working as
    designed. Additive on the existing action, so clients predating the field
    hold exactly as they did; both ports read it
    (`a_hold_says_when_to_ask_again_and_no_room_says_later`).
- **OPEN — decided in principle, deferred deliberately.** When this is built
  it is a **local rebuild**, not peer hydration: hydration means a new
  protocol, a transfer path, a trust boundary and a partial-transfer failure
  mode, all to avoid re-deriving something from a source the node already has.
  It stays deferred until §4, because the prepared transaction changes what a
  takeover means and building recovery against the current shape risks
  building it twice. VOD owner loss is classified
  `Unrecoverable` by design (`media_sessions.rs:2580-2605`), pinned by
  `a_route_the_takeover_path_can_never_accept_is_unrecoverable`. The only
  hydration machinery that exists is for fragment-index blobs, not for
  rendition ownership. The peer-hydration-versus-local-rebuild question is
  still open in the review.
- **PARTIAL — the named defect is present verbatim.**
  `playback_control.rs:1111-1115` still matches `(SessionKind::Transcode {..}, _)`
  and retains transcode for every quality value, Original included. The copy
  side is done and tested; the transcode side is not, and no test does
  Original → 720p → Original against a transcode predecessor.
  **Read `d4770238` before attempting this again.** An implementation was
  written and withdrawn, and the reasons are recorded rather than the attempt:
  `candidate_request` cannot reconstruct a copy recipe from a transcode
  predecessor, so it would invent `aac`/`preserve_dolby_vision`/
  `convert_dolby_vision` — and for a converting-copy Profile 7 title the
  invented answer is raw dual-layer P7, the one delivery nothing plays. The
  recorded real fix is to plumb `review_client_plan`/`apply_plan_review` into
  `process_preparation_candidate`, which belongs with §4's client work rather
  than ahead of it.
- **PARTIAL.** Browser resolution is done and the run is proved terminal: the
  Playwright action exports `PLURX_PLAYBACK_CHROME`, `playback-lab` honours it,
  the nightly point is `missing = "fail"` rather than skip, and
  `enforceCaseResult` turns any non-terminal status into a failure. Still open:
  the strongest cases (steady, seek-storm, stall-recovery) are deliberately
  excluded from the PR gate (`ci.yml:545-555`); evidence retention is 3-14 days
  and CI-scoped.

  The durable action/outcome ledger exists now. `durable_outcome` writes a
  terminal, a retry backoff, a withheld recovery and a suppressed action into
  the node-local `playback_events` table, so a refusal outlives the process
  that made it. Deliberately not every exchange — one happens every few
  seconds per viewer, and a ledger recording the healthy path is one nobody
  reads. Evidence retention for browser runs is still CI-scoped and the
  strongest cases are still outside the PR gate.

  `scripts/ship --status` closes the last of those. It asks every node in the
  inventory for `/readyz` and `/metrics` and prints build, readiness,
  cache-hit GETs in flight, and blocked GETs against their cap — then names
  any node that did not answer rather than omitting it, and warns when more
  than one build is serving, because a partial deploy is invisible in a
  per-node list nobody adds up. The daemon already served every one of those
  facts; this is the first thing that reads them. Read-only by construction
  and tested as such (`tests/operations/test_ship_status_probe.py`).
  **Encoder capability is still not probed.**

### 4. Executable prepared transaction and three client adapters

> **Closed in current source, 2026-09-09.** The warning retained below
> describes the preimplementation baseline. Current VOD serving resolves
> encoded recipes, all three clients enforce contiguous runway against the
> incumbent's current film position, and `codex/m6-server-prime` reserves and
> attaches the successor before the actor announces it. Settings → Developer
> exposes the direct default-on server checkbox; the readiness list is
> advisory and is not consulted by the attach path.

> **The successor's worker is blocked on a device measurement, not on code.
> Read this before planning any of §4's remaining work.**
>
> Staging starts no worker — `ControlAction::Prepare`'s own doc says *"This
> slice deliberately starts no worker behind that route"* — so a client that
> is offered a successor cannot fetch it. Closing that looks like server work.
> It mostly is not, and the reason is three facts that stack:
>
> 1. `PREPARED_AXIS_SETS` admits exactly two crossings: `{ResolutionOrBitrate}`
>    alone, and that paired with `{DeliveryMethod}`.
> 2. `rendition_key` (`vodserve.rs`) hashes file id, source identity, audio
>    index, AAC, the two Dolby Vision flags, audio offset and cluster cache
>    key. **Not height. Not bitrate.** Two Copy recipes differing only in
>    resolution or bitrate therefore share one rendition — the same producer
>    and the same bytes. On a Copy source the delivered media *is* the source,
>    so `{ResolutionOrBitrate}` alone is not a transition in delivery terms;
>    there is nothing to switch to.
> 3. The pair that does change the media — Copy→Transcode, which is what the
>    2026-09-03 hardware run measured — is refused by immutable-VOD serving:
>    `try_create_with_release_fence` answers `501 vod_transcode_unavailable`,
>    *"transcode serving is gated on the D6 device measurement"*.
>
> So the worker can be built, and for every case VOD may legally serve it
> would produce the identical stream the viewer already has. **Transcode-rung
> VOD stays unavailable until the open P2/D6 measurement establishes that
> AVPlayer and Media3 tolerate the planned EXTINF timing**
> ([PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md)) — an Apple TV and an Android TV
> in a room, not a PR. Do not spend a week on the plumbing expecting to
> demonstrate a switch at the end of it.
>
> **Two findings to keep, for whoever does build it after D6 lifts.**
>
> **Publishing the staged row is a fork, and the cheap-looking side is the
> wrong one.** `classify_durable_route` reads four fields and never joins the
> pointer, so clearing `publication_ready_at_ms` makes a staged route fully
> playable while the pointer still names its predecessor — no new gate needed.
> But the sentinel is also what keeps the row out of `owned_media_sessions`
> and out of the retirement sweep. Clear it and the 3-second lease loop adopts
> the row and renews `lease_expires_at_ms` to a 12-second TTL, overwriting the
> 330-second deadline the preparation ledger mirrors — the exact thing
> `MediaSessionPreparation::deadline_ms` forbids in its own words: *"Two clocks
> over one row is how a staged generation ends up half reaped."* Worse, a
> published row whose worker stops being `live` becomes a stale-settlement
> candidate and is ended within one tick, so a slow client's commit then fails
> the CAS and reports a rejected acknowledgement for a successor **the server
> killed while the viewer did exactly what it was told**. The recorded
> preference is therefore the other side: leave the sentinel, and teach
> `classify_durable_route` one additional case for a route that is the
> currently-staged successor of this playback — with the extra read taken only
> when the route would otherwise be refused, so a serving route still
> short-circuits on `publication_ready_at_ms == 0` and the hot path is
> unchanged.
>
> **There is a fourth teardown path nobody has enumerated.** Abort, deadline
> lapse and owner death are all durable-only and none of them stops a worker.
> On top of those, `SESSION_IDLE_TTL` is 300 s while `PREPARATION_DEADLINE_MS`
> is 330 s, so a successor the client never fetches is idle-reaped **thirty
> seconds before its own deadline** — and that reap is deliberately
> tombstone-free (*"an idle reap is the one ending a session may come back
> from"*), so `Session::abort_staged_preparation` never runs and nothing clears
> the slot. The reap also acts on the *successor's* session, whose control
> state holds no slot; the slot is on the predecessor. In that 30-second window
> the offer still validates and a commit still succeeds, onto a successor with
> no worker. Any slice here must release the worker on all four paths.
>
> **Also do not write the acceptance on `install_http_test_session`.** Every
> existing `stage_prepared_successor` test runs against the *rolling* gate, not
> `VodPreparationGate`, because the fixture registers into the rolling map —
> the same miss `PreparationGate`'s own doc predicted once already: *"a gate
> that existed only on the actor would let M6 stage successors on a path
> viewers do not take, and its acceptance would pass while the feature fired on
> nothing."* The only assertion this gap's definition supports is fetching a
> real segment from the successor's playlist while the pointer still names the
> predecessor.

Retain the existing durable ledger; do not build a second competing transaction.
Create a real candidate worker with capacity ownership, immutable recipe/codec
metadata and bounded exact-candidate priming reads. Never remove the global
publication barrier merely to let a staged candidate fetch media.

Implement **Prepare → client Ready → durable server commit → client Switched
→ bounded predecessor drain**. Current `Committed` acknowledgement requires
first-frame time and then commits Store; that is not the target ordering.
Bind every offer/readiness/receipt/switch to candidate, controller lifetime,
desired recipe/destination, actual media origin, readiness revision, aligned
film-time window, owner epoch and absolute resource deadline. Wall-clock first
frame time is diagnostic, not proof of ownership/freshness/display.

Keep **desired C**, **displayed A**, and **server-active/prepared B** separate.
C after durable B commit but before visible switch must reconcile against B;
A may remain displayed only under bounded drain and must never regain server
authority. Lost commit responses require exact durable replay. A postcommit
display failure starts a new fenced recovery, not an implicit rollback.

**Where the server side actually attaches — read out of the code, 2026-09-07.**
The next slice is `Switched` and the bounded drain, and the seams for both
already exist. Recording them so the work starts from the code rather than
re-deriving it, and so the three adapter sessions can run in parallel against
a settled contract instead of three guesses.

- **The offer is already bound.** `PreparedActionBinding` holds the
  `PreparedSuccessorAction` beside the `Prepare` it sent, and
  `bound_preparation_acknowledgement` already compares `action_id` and — since
  `b1ba4f3f` — `committed_media_origin_ms` against
  `binding.successor.media_origin_ms`. Every further field in §4's binding
  tuple attaches at that one comparison. It is the right place: it is the only
  point that holds the offer and the answer at once.
- **`AcknowledgementState` has no `Switched`.** Today it runs
  `MetadataReady → BufferReady → Committed → Failed | Aborted`, so the durable
  commit is also the last thing the client says. That conflates *committed*
  with *the viewer is seeing it*, which is exactly the distinction §4 exists to
  draw: `Committed` is the server's transaction, `Switched` is the client's
  display. A client that commits and then fails to present must be
  distinguishable from one that succeeded, and today it is not.
- **The predecessor is retired by the commit's own SQL, and that is the real
  obstacle.** `commit_media_session_preparation`
  (`plurx-core/src/store/sqlite/sessions.rs:2084`, replicated twin at
  `hiqlite_sessions.rs:2352`) sets the predecessor to `state = 'ended',
  terminal_reason = 'superseded'`, deletes its `cache_consumer_pins` and
  collapses its `job_leases` — **inside the same transaction that advances the
  pointer**. So "retire the predecessor after the client switches" is not a
  new directive layered on top; it requires that transaction to stop ending
  the predecessor, and something else to end it later.

  This was attempted on 2026-09-07 as a `Switched` acknowledgement state plus
  a `ReleasePredecessor` directive, and **withdrawn before commit** because it
  released nothing. Recorded so it is not attempted the same way again:

  - The commit SQL above already did the retirement, so the new directive
    changed no behaviour at all while its doc comments claimed it did.
  - The guard was inverted. `PreparationSlot::Committing` means the CAS has
    *not* yet decided; after a successful commit `settle_preparation` sets the
    slot to `Empty`, so the directive was unreachable in production and the
    test passed only by parking the slot artificially.
  - A post-commit `Switched` cannot arrive anyway: the predecessor route is
    `ended`, so `control_inner` answers 410 before the actor sees it, and the
    successor's actor has no `prepared_action` to bind it to.
  - `AcknowledgementState` has no `#[serde(other)]` fallback and
    `PROTOCOL_V1` was not bumped, so an older owner rejects `"switched"`,
    returns an empty 400, and the ingress maps that to a 503 with
    `retry_after_ms` — a client wedged in a retry loop that never converges.
    Whatever adds a state needs the negotiation `supported_actions` already
    does for the server-to-client direction.
  - The `demand == End` fence at `playback_control.rs:278` covers `Committed`
    only, and a new terminal-ish state needs adding there.
  - `first_frame_unix_ms` is required on `Committed`, which by this design is
    the state where nothing has reached the screen yet. It belongs on the
    state that means a frame was displayed.

  So the order is: change what the commit transaction retires **first**, then
  add the client state that gates the later retirement. Adding the state first
  produces a protocol change that reads as progress and does nothing.

  **The schema already permits the deferral — checked, 2026-09-07.** This was
  the thing that could have made the whole approach impossible, and it does
  not: `media_playback_pointers` is `PRIMARY KEY (user_id, playback_id)` with
  `current_incarnation_id` UNIQUE, so exactly one incarnation is *current* per
  playback — but `media_sessions` carries no uniqueness on
  `(playback_id, state)`. Two sessions may both be `active` for one playback;
  the pointer, not the state column, is what makes one of them current. A
  predecessor can therefore stay `active` while the pointer already names the
  successor, which is exactly the window §4's drain needs. No schema change is
  required.

  **BUILT AND WITHDRAWN, 2026-09-07 — the lease is the wrong thing to bound a
  drain with, and the paragraph below is the first of three reasons why.** An
  implementation reached green contracts on both backends and three
  independent adversarial reviews each found a different P0 in it. It is not
  in the tree. What is in the tree is this record, because the next reader
  will otherwise reach the same design a fourth time.

  The shape that was tried: the commit stops retiring the predecessor and
  writes it a lease ceiling instead; the drain is bounded by a rule in
  `renew_media_sessions` about which incarnation the playback pointer names.
  Every variant of that rule fails, and they fail for the same underlying
  reason — **the owner's lease loop already owns the meaning of a lease, and a
  drain is not a lease.**

  1. **Refusing the renewal kills the worker in one tick.** The paragraph
     below says the maintenance sweep bounds the drain. It cannot:
     `renew_media_sessions` consults no pointer, and neither does
     `owned_media_sessions`, which is what feeds the owner's lease loop. The
     old commit hid the predecessor from all of these at once, and it did so
     three times over: `state = 'ended'`, a lease collapsed to `now`, and the
     blocked publication sentinel. **Every one of those four readers filters
     `state = 'active'`, so any one of the three would have hidden the row —
     and the withdrawn attempt cleared none of them.** (An earlier version of
     this paragraph credited the sentinel alone. That is wrong, and it is
     wrong in a way that matters: `begin_removal_attempt_sql`, named below as
     a third pointer-free reader, has no sentinel predicate at all, so for
     that reader `state = 'ended'` was doing all of the work.) So the owner
     keeps offering the predecessor, reads an omitted renewal as `cluster
     lease lost`, and fences and kills the worker on its next three-second
     tick, while the store still says `active`.
  2. **Clamping the renewal only delays it by about eight seconds.** Make the
     renewal succeed at an unmoving value and the loop still drops the route
     from its batch at `ceiling - LEASE_RENEWAL_MIN_REMAINING_MS` (4 s) and
     fails it closed down the same `cluster lease lost` path. Every healthy
     prepared switch then emits the split-brain log line and metric. And after
     that reap the row is a zombie: `fence_and_reap_sessions`'s VOD arm handles
     `ended` and missing, so an `active` route falls through it writing no
     terminal and stopping no worker. (It does begin a publication fence
     before the match, so the viewer stops receiving media immediately — the
     zombie is the row and the encoder behind it, not the delivery.) The row
     then sits with a lapsed lease until the maintenance sweep — `now - 60 s`
     on a five-minute tick, so up to six minutes.
  3. **"Absence of a pointer row renews normally" makes the session
     immortal.** That rule is needed because a session mid-creation has no
     pointer yet. But `end_media_session` *deletes* the pointer row, which is
     what happens when the viewer closes the tab. Sequence: commit at t=0,
     viewer stops at t=2 s, pointer deleted; at t=3 s the predecessor is still
     `active` with a live lease and a live worker, the clamp finds no pointer,
     and it renews. Forever. An encoder, one of two admission permits and a
     shared-cache generation, held for a stream nobody is watching, until the
     process exits — and node removal refuses the whole time
     (`membership.rs` `begin_removal_attempt_sql` is a **third** reader with no
     pointer predicate, alongside `expired_media_sessions` and
     `claim_media_session_takeover`).

  **What the design has to be instead: the drain is durable state that the
  owner acts on, and the lease machinery is not touched at all.**

  The failure common to all three attempts is that "this session is draining"
  was *inferred* — from the pointer, through the lease. Inference is what broke:
  the pointer is deleted by an ordinary user action, and the lease already
  means something else to the loop that owns it. Write the fact down instead.

  - **Schema.** One nullable column, `media_sessions.drain_deadline_ms`
    (SQLite 49 → 50, replicated AUTH 29 → 30). Null means "not draining", which
    is every row that exists today, so the migration is additive and an old
    binary reading the table is unaffected.
  - **Commit.** Stop ending the predecessor. Stop deleting its pins. Stop
    collapsing its `job_leases`. Set `drain_deadline_ms = now + DRAIN_MS` and
    change nothing else. State, cause, publication readiness and lease are
    untouched, so `owned_media_sessions`, `renew_media_sessions` and the reap
    classifier all keep working exactly as they do now — which is the whole
    point, because every attempt that touched them broke something.
  - **The owner ends it, on the tick it already runs.** The lease loop already
    walks `owned_media_sessions` every three seconds. Any owned row whose
    `drain_deadline_ms` has passed gets `end_media_session_if_owner` with cause
    `superseded` — the right cause, the ordinary teardown, the pointer delete
    already guarded on the exact incarnation so it cannot take the successor's.
    No new timer, no spawned task to lose across a restart.
  - **`Switched` ends it early.** Step 2 calls the same function from the
    acknowledgement instead of waiting for the deadline. That is the only
    difference between the two steps, which is why they are one piece of work.
  - **The sweep is the cross-node backstop.** `maintain_media_sessions` ends
    any row whose `drain_deadline_ms` has passed, whoever owns it, so a node
    that dies mid-drain cannot leave one behind. Its five-minute tick is a
    backstop interval, not the bound; the owner's own three-second tick is the
    bound, and it survives a restart because the deadline is on the row rather
    than in a task.
  - **Takeover excludes a non-null `drain_deadline_ms`.** Durable, so it holds
    when the pointer is gone — which is exactly the case that defeated the
    pointer-based exclusion.

  Check against the three failures above: (1) nothing about renewal changes, so
  no refusal is read as lease loss; (2) nothing is dropped from the renewal
  batch early, so the worker lives the whole window; (3) the drain does not
  depend on a pointer row existing, so deleting the pointer changes nothing.

  **Things this design must still be held to.**

  - ~~The drain window has to be shorter than the `cache_consumer_pins` TTL,
    because a draining predecessor's pins are renewed by no statement that
    survives the commit.~~ **Withdrawn — this was carried over from the
    withdrawn design and is not true of this one.** `renew_media_sessions`
    renews the pins of every session whose lease update succeeded, in the same
    call, and this design keeps the predecessor `active`, unsentineled and
    renewing for the whole drain. Its pins ride the renewal it has always had.
    The constraint was real only where renewal was refused or clamped. Do not
    add a pin-renew path for the drain, and do not expect the sweep to collect
    a generation it cannot.
  - A `Switched` that arrives after the deadline has already ended the row
    must be idempotent, not an error — `end_media_session_if_owner`'s CAS
    answers that, but the acknowledgement path has to treat "already ended" as
    success.
  - **The predecessor can now need a second receipt, and the table holds one
    per session.** `media_session_terminal_acks` is `session_id PRIMARY KEY`,
    and the commit transaction writes the commit receipt under the
    *predecessor's* session id. Before the drain that was harmless because the
    same transaction ended the predecessor, so no second receipted exchange
    could reach it. A draining predecessor stays live and answers the
    `demand: end` every shipped reporter sends when the viewer closes the
    player: the write loses to the commit receipt, the caller reads that as
    "not durably committed", and the client gets a `503 control_unavailable`
    it retries twice a second for the rest of the window.

    ~~The retained reply has to become the most recent receipted exchange
    rather than the first one.~~ **Built that way, then withdrawn under
    review, because it is worse than what it replaced.** Every reader of that
    row demands byte-exact equality with what the commit wrote, so a commit
    whose response was lost and is replayed after the viewer closed the player
    reads back non-exact and is classified as *refused* — a rejection
    tombstone for a successor the pointer already names. And the ordering rule
    it needed was wrong: `sequence` is monotone per *client instance*, not per
    session, so a reloaded page starting again at one either starves or
    clobbers depending on which side of the stored number it lands.

    **What shipped instead: a terminal exchange on a draining predecessor is
    not receipted at all — it ends the row.** The retained reply exists so a
    lost terminal answer can be replayed; once the row is `ended`, a client
    that missed the answer asks again and gets `410 session_ended` from the
    row itself, which is the same terminal by a shorter path. The commit keeps
    the receipt slot it needs for its own replay, and the route carries
    `drain_deadline_ms` so the control plane can tell the two cases apart.
    This is also most of what step 2's `Switched` was for: the ordinary case
    releases the encoder and its admission permit immediately rather than
    waiting out the window.
  - **An old binary must not be able to take a draining session over, and SQL
    predicates cannot make that true.** Compatibility is checked when a store
    is opened and never again, so during a rolling upgrade every node still
    running the previous binary keeps issuing takeover SQL that has no drain
    predicate in it. A new-binary owner that dies mid-drain is then adopted by
    an old-binary node, which starts an encoder for a stream the pointer no
    longer names and renews it forever — failure 3 arriving through the
    upgrade door. This needs a trigger, the way the v49 pointer fence did, and
    the trigger has to ship in the same migration step as the column.

  Step 2 then replaces "after the window" with "when `Switched` arrives, or the
  window lapses", which is the same call at a different trigger.

  **Step 2's server half has landed. Its client half must not follow in the
  same release, and this is the part the first attempt got wrong.**
  `AcknowledgementState` has no unknown-value fallback, so a `switched` sent to
  an owner that does not know the word is a deserialize failure, an empty 400,
  and a 503 with `retry_after_ms` that the client retries forever. During any
  rolling upgrade some owners are older than some clients. So the sequencing is
  fixed and is not a matter of taste:

  1. **This release** teaches every node to accept `switched` and act on it —
     it releases a draining predecessor early. No client sends it, so nothing
     can wedge.
  2. **A later release**, once every node in the fleet runs a binary from step
     1, teaches clients to send it.

  **How a client will learn which owner it is talking to is step 2's problem,
  and it is not solved by adding a response field.** The first attempt at this
  advertised the vocabulary as `ControlResponseV1.accepted_acknowledgements`
  and had to be withdrawn before merge: `ControlResponseV1` carries
  `#[serde(deny_unknown_fields)]` and is re-parsed by the *relaying* node in
  `validated_control_relay_response`, so a new field on it is not additive at
  all — during a rolling upgrade an older ingress relaying to a newer owner
  fails to parse the reply and answers `503 control_unavailable`, and the same
  applies to a `RetainedTerminalResponse.response_json` replayed after a
  rollback. Any negotiation field therefore needs `deny_unknown_fields`
  relaxed, shipped alone, and rolled out fleet-wide *first* — which is three
  releases, not two, and is why none of it belongs in this one. Alternatives
  worth weighing when step 2 is picked up: relaxing the attribute on its own;
  putting the vocabulary in the start response, which is minted by the owner
  and not re-parsed by a relay; or having the client discover it by sending
  `switched` once and treating a `400` as "this owner is older", which needs no
  wire change at all but costs one wasted exchange per session.

  What step 2's client half is worth, now that `demand: end` already releases
  the drain: the client that switches and keeps the tab open. That client tears
  nothing down, so without `switched` it pays the full window — and the window
  is one of two admission permits.

  ~~**Land step 1 and step 2 together.** Step 1 alone makes the product worse
  for the window it opens: today a superseded predecessor answers a clean
  terminal `410 session_ended`, and a half-built drain replaces that with a
  retry or a `owner_lost` for a client that has no `Switched` to send.~~
  **Withdrawn — that was a property of the lease-bounded drain, not of this
  one.** Nothing in the request path consults the pointer: `control_inner` and
  the manifest and segment paths all resolve authority from the row named by
  the URL, so a draining predecessor answers *normally* for the window and
  then, when the owner ends it with cause `superseded`, answers the same clean
  `410 session_ended` it answers today — just later. `owner_lost` and the
  retry came from fencing the worker while the row still said `active`, which
  is what this design stopped doing. Step 1 is shippable on its own; step 2 is
  what stops every switch paying the full window when the client has already
  moved.

  **To settle before the candidate worker lands.**
  `DEFAULT_MAX_HW_SESSIONS` is 2 and an admission permit is held for a
  session's whole life, so prepare-plus-drain with a real successor costs two
  of two per viewer — the first real cutover on a two-slot node refuses its own
  successor. This is the strongest argument for step 2: without a `Switched`,
  every switch holds the second permit for the whole window whether the client
  needed it or not.

  **Landed from that work, because it is correct on its own:**
  `preparation_ack_replay` no longer gates on `state = 'ended' AND
  terminal_reason = 'superseded'`. That was the same fact as "this commit
  already happened" only while the commit retires the predecessor; the receipt
  is the real fence and a stricter one, comparing incarnation, owner, epoch,
  client instance, sequence and request fingerprint, so only the byte-identical
  request that produced a receipt can replay it. Doing this first means step 1
  cannot silently break lost-commit replay when it lands.

  **Two stale numbers while we are here:** SQLite schema is **49** and
  replicated AUTH schema is **29** (next available 50/30), not 47/27.

  **And the bound the drain needs already exists: the predecessor's lease.**
  The maintenance sweep at `sessions.rs:3372` ends any session with
  `state = 'active' AND lease_expires_at_ms <= ?`, in batches, without
  consulting the pointer. That is currently harmless because the commit ends
  the predecessor first — but the moment it stops doing so, this sweep becomes
  the thing that reaps a predecessor whose client never reported a switch. So
  "bounded predecessor drain" does not need a new timer or a new sweep; it
  needs the commit to stop ending the predecessor and to stop renewing its
  lease, and the existing sweep provides the bound.

  That is worth stating precisely because an earlier version of this note said
  "any sweep that reaps by `owner_node_id` and `state = 'active'` needs a
  pointer exclusion", which is broader than the code justifies. Checked
  individually: every other `state = 'ended'` write in that file targets an
  exact `incarnation_id` or a named staged preparation, so none of them can
  reach an unrelated draining predecessor. The two counters at
  `sessions.rs:446` and `:970` differ over the pointer, but both are admission
  capacity, and a draining predecessor genuinely is consuming a node's
  capacity — counting it is right, not a leak.

- **The bounded-drain machinery already exists at
  `settle_activation_predecessor`**
  (`http/hls.rs:3083`), bounded by `PREDECESSOR_PROJECTION_FAST_WINDOW` (5 s)
  with `PREDECESSOR_PROJECTION_RETRY_DELAY` behind it, and able to report
  `PredecessorAcknowledged`. It has exactly one caller,
  `activate_session_under_authority` (`:2891`) — the activation path — so it
  is not currently reachable from a prepared transaction at all. It is the
  right machinery to reuse once the commit stops doing the retirement itself;
  it is not a seam that can simply be gated differently today.
- **`activate_media_session` must stay out of this path.** It reaps the
  predecessor and advances the pointer unconditionally;
  `a_prepared_transition_stages_a_successor_and_leaves_the_pointer_alone`
  exists to pin that `prepare_media_session` is the entry point instead. Any
  drain work has to keep that test true.

The order that follows from this: add `Switched` and gate the existing drain
on it, *then* fan out the three adapters. Doing the adapters first means three
clients inventing three meanings for "ready" and "switched" against a server
that has not yet fixed either, and the disagreement only surfaces at
integration.

Adapter seams from the read-only audit:

| Client | Current behavior | Required implementation |
|---|---|---|
| Apple | One AVPlayer; `PlayerController.open` releases predecessor before replacement; Caps advertises dual preparation from hardware experiments | Candidate player/item, role-owned observers, explicit surface/audio/PiP switch and delayed release; truthful implemented capability |
| Android | Plan-scoped Compose PlayerContent/Controller releases old player/session; one player's `setMediaItem` | Coordinator above plan replacement, active/candidate lifetimes, one system audio-focus/MediaSession owner |
| Web | Network preparation retains predecessor, but `retireOutgoing`/`attachHls` destroy it before successor decode; one `#video` | Two media pipelines, role-bound callbacks and explicit visible/audio switch; refactor global PLAYER attribution |

All reporters currently name only hold/retry_resource/terminal; no production
standby adapter consumes Prepare. Add vocabulary/models with actual execution,
not an unsupported capability claim. At final switch revalidate the latest
transport: a future cutover cannot jump a paused viewer. Resource TTL stays
finite during long Pause/background even if active presentation timing pauses.
Also finish whole-pipeline cold startup and persistent recovery controls on
all three clients: Apple's landed deadline bounds the initial decision only,
not create/attach; the capture repair does not finish every cold input axis.
Prove retained complete recipe, seek and Pause through failure/Retry/Close,
including native/overlay subtitle awaits and delayed old attachment callbacks.

### 5. Visible enablement, qualification and promotion

**Landed — the readings, not the whole item.** `GET /api/v1/developer/readiness`
(`http/developer.rs`) answers every prerequisite the Developer cards list with
`met` / `unmet` / `unobservable` plus the sentence it read, and the tab renders
them in place. Four things to know before extending it:

- **`Unobservable` is the common answer and must not be relaxed.** Most of
  these questions are not answerable from a running daemon: a CI receipt, a
  roster a single-node install does not have, a fleet of clients the counters
  can only see once they report. Two adversarial reviews found five rows
  claiming otherwise in the first draft — including a green tick built on
  `cache_admin_revocation_ready()`'s deliberate fail-open for an unreplicated
  node. If a row you are adding can only ever be `Met` by rounding, make it
  `Unobservable` and put the numbers in the evidence.
- **The requirement copy stays in `index.html`; only the reading comes from the
  daemon.** The server never starts owning UI prose.
- **The advisory rule is pinned from both sides, and the first version of both
  pins was blind.** `an_unmet_prerequisite_does_not_block_the_switch` must
  drive a prerequisite that is genuinely `unmet` — it originally drove an
  `unobservable` one, which a gate keyed on `Unmet` would have walked past —
  and the web assertion compares the whole panel with the reading spans
  stripped, because the earlier per-control comparison used a stub that dropped
  the argument a `disabled` flag travels in.
- **A row the route does not report renders "not reported", not "checking…".**
  `the panel asks for exactly the rows the server reports` keeps the two sides
  in step.

Rows that are claims about the build rather than the deployment need a pin at
the code they describe, or they go stale silently: `server_preparation_is_real`
has `a_prepared_successor_still_publishes_a_staged_encoder` in `http::hls`, and
that test is what will tell whoever finishes §4 that this row now lies.

**Landed — the hidden gates around the retained live engine.** The
`live-hls-recovery` Cargo feature is gone. It was a *default* feature that
nothing in the Makefile, Dockerfile, deploy or scripts ever turned off, so it
decided nothing while hiding ~100 code sites and a whole streaming engine
behind a compile flag; whether the engine runs is a runtime setting an operator
can see, and whether it exists is not a question a build should answer
silently. Three things came with it:

- **The test and production defaults were opposites.** `live_hls_recovery_enabled`
  read `== Some("1")` under `cfg(test)` and `!= Some("0")` otherwise, so every
  VOD-refusal regression in the repo described a policy the fleet does not run.
  Four tests changed answer when the two were unified — including one whose own
  comment said *"The old fourth method is not a fallback"* about a path that is
  exactly a fallback in production. Each of them now states which policy it is
  asserting, and
  `the_shipped_default_answers_a_vod_refusal_with_the_retained_engine` covers
  the default that had never once run.
- **The engine is attributable now.** It transcodes, so a session it serves
  costs encode time on the node, and it announced that in a `tracing::warn!`
  and nowhere else. `plurx_live_hls_recovery_sessions_total{reason}` counts
  what it served and why, and a Developer card reports the same numbers and
  names the switch that stops the fallback. The switch itself stays in
  Playback → Streaming; two copies of one control drift.
- **One path does not consult the switch, and that is now settled rather than
  open.** A request arriving already stamped `Presentation::Live` goes straight
  to the retained engine — today the only producer of one is peer takeover,
  whose `takeover_recipe_is_valid` *requires* that stamp, while public and
  worker ingress both require `Presentation::Vod`. It is counted under
  `requested_live` and reported by `no_session_bypasses_the_switch`, because a
  node serving live-HLS sessions with the fallback off is otherwise invisible.

  **Decision (Paul, 2026-09-08): takeover consults the setting and refuses,
  and the refusal names the switch.** `attempt_takeover` now answers *"live HLS
  fallback is disabled cluster-wide, so this session cannot be adopted"*, right
  after the EVENT gate.

  Two earlier framings of this were wrong and are worth keeping so nobody
  re-derives them. The note first left the question open on the premise that a
  node might be configured differently from its cluster; it cannot be, because
  settings are replicated. What it was actually describing is a *time* gap — a
  session that started while the fallback was on, kept running when it was
  turned off, and then outlived its owner. The second framing used that
  narrowness to argue for adopting anyway: the session exists, someone is
  watching, and refusing ends a film rather than preventing a stream. **The
  rule beat the trade.** The cluster does not do what a setting says it may
  not do — a system that quietly makes exceptions to its own configuration is
  one an operator cannot reason about, and the rarity of the case is an
  argument that the rule is cheap, not that it can be skipped.

  A store read that fails is a skip rather than an adoption, so the next sweep
  asks again instead of proceeding on a value nobody read.

  This was the fourth reader of `playback.vod_live_recovery`, and adding it is
  what settled the parse: the engine, the settings DTO and the Developer card
  each compared the raw string against `Some("0")`, with the card's comment
  pinned to that on purpose so the row could not report a switch the engine was
  not honouring. All four share `stored_switch` now, which keeps that pin.

Not done here: removing the engine. §5 conditions that on real VOD recipe
coverage replacing it, and the coverage row exists precisely so someone can
tell when that is true.

Still open here: the qualification/promotion run. The hidden-gate inventory
below is closed — every entry in it now has a visible control — and is kept
for what each one cost, because the shape repeats.

**The hidden-gate inventory**, found while doing the above. Each was a switch
that changed product behaviour with no visible control:

- ~~`PLURX_PGS_OVERLAY`~~ — **done.** It is `subtitles.pgs_overlay`, read at
  the request boundary, with a Settings → Developer card carrying the switch
  and what has to be true first. The variable seeds the setting once on a node
  that has never been told either way and does nothing after that.
- ~~`PLURX_DV_CONVERT`~~ — **done.** `playback.dolby_vision_convert`, same
  shape. Worth knowing for the next one of these: moving the switch is the
  easy half. The expensive defect was a *reader left behind* — the fragment
  indexer still read the boot value, so turning the conversion on gave a
  decision path that named a converting identity nothing would ever build, and
  it healed only on restart. Grep for every reader of the old boot-latched
  value before believing the move is complete, and make one parser own the
  stored string: three parses of it meant a hand-edited `TRUE` served overlays
  while the card said off.
- ~~`OFFLINE_ENABLED`~~ — **done.** It had a settings-API field and no
  control, along with the three offline budgets beside it; all four now render
  as an Offline downloads card in Maintenance. The same three-parsers defect
  #141 found in the overlay switch was here too — the DTO, the preparer and
  the offline API each ran their own negated `matches!`, none trimming or
  folding case, so a hand-written ` OFF ` read as *enabled* at all three. One
  `stored_switch` now. Two of the three readers are reachable from a request
  and are asserted together in `http/mod.rs`; the preparer is pinned where it
  lives, in `offline.rs`, because its only existing test stored lowercase
  `off` — a value the old parser also read as disabled — so that reader could
  have been left behind with the suite green.
- ~~`SW_POOL_THREADS`~~ and ~~`MAX_HW_SESSIONS`~~ — **done.** Both are fields
  on the settings DTO and selects in Playback → Streaming. The round-trip test
  ends at `TranscodeManager`, not at the DTO, because the expensive half of
  moving a switch is always the reader. Three things the adversarial pass on
  that PR established, each of which had been guessed wrong first:
  - **A stored zero is reported and writable on both keys**, because both
    readers honour it. The first draft refused a zero software pool on the
    theory that `try_admit_software` admits nothing against a budget of zero.
    It does not — `SwPool::try_take` grants unconditionally while the pool is
    empty, deliberately, so a two-core box is not banned by its own budget.
    Worse, refusing it made the *whole Streaming card* unsavable on any node
    that already held a zero, because the DTO reports a stored value verbatim
    and the page sent the card whole. **A control must be able to write back
    every value its own page can display.**
  - **A zero hardware cap is not a software-only mode**, and must not be
    offered as one. `admit_live` still takes the hardware branch whenever the
    node has an encoder, so every start queues for a slot that never frees,
    waits out the full five-second admission window and only then falls back —
    and a 4K HEVC HDR stream, which software cannot keep up with, is refused
    outright with "all 0 hardware transcode slots are in use". The mechanism
    for software-only is `keys::HWACCEL`.
  - **The software default is per-node and the setting is replicated.** The
    page therefore sends either capacity field only when an operator actually
    moved it; a card that always wrote them would give a 4-core node a 16-core
    machine's budget from an edit about the buffer limit.
  **`LIBRARY_DV_DISK_CONVERT` is not one of these** — it has a
  dedicated API in `http/dv_disk.rs` and a per-library select in the web UI
  (`dvModeSelect`). It was listed here in a first draft and is recorded as a
  correction so the next reader does not build a control that already ships.

`PLURX_HWACCEL` is the pattern the rest should follow: it seeds a stored
setting the admin UI then displays, rather than being an invisible decision
input at each call site.

Finish the Developer Enable section's prerequisites and actual behavior;
remove retained compile/default hidden live-HLS fallback once real VOD recipe
coverage replaces it. Do not introduce another hidden switch.

Freeze task merges, integrate then-current main, review the exact final tree,
run/fix the unit suites, and run one complete main-promotion qualification on
the final fixed candidate. Require **Main promotion gate** and inspect its
exact-tree qualification receipt. If candidate/base changes, evidence is stale.
Use the detailed review's playback SLOs and failure matrix: Chrome/Safari,
iPhone/Apple TV, Android phone/Google TV, two-hour 4K, seek/quality storms,
capacity and node loss, actual frame timing and audible gaps. Unit/simulator
passes are not physical-device qualification. Merge only proven current heads;
clean up agent branches/worktrees after preserving stable evidence links.

## Approval boundary — do not silently work around it

The proposed serving-proof continuity change was rejected by automated safety
review as a cluster authority-boundary change. **No patch was applied.** Obtain
explicit user approval before implementing it; a general “keep going” is not
that approval. Other tasks can continue.

**Approved and implemented, 2026-09-05.** The user was shown the change and the
five constraints below verbatim, asked whether to build it, and answered yes.
That is the explicit approval this section requires — recorded here rather than
only in a commit message, because the next reader of this paragraph needs to
know the boundary was cleared by a person and not reasoned around. The
constraints were implemented as written; the paragraph below stands unchanged
as the specification they were held to.

The reviewed proposal retains an already-eligible quorum proof only until its
**original** expiry when a newer watermark is not locally applied. Never extend,
restamp or renew the old proof because local apply caught up. Prefer a new
eligible proof; invalidate on term/leader/epoch conflict and required errors.
Recheck current proof-refresh/lease/election timing and mixed-version minima.
Already-open immutable HLS bodies have bounded drain; do not impose a short
whole-body HLS deadline on a legitimate multi-hour direct-file stream.

## Verification — resume the warm local loop

Pinned `rustc` is **1.97.1 (8bab26f4f 2026-07-14)**; Homebrew's default is
older. Verify explicitly. Shared target:
`/private/tmp/plurx-streaming-reliability.1WSd2I/repo/target`.
The VOD author uses a separate warm target:
`/private/tmp/plurx-streaming-server.5u1DYC/repo/target`.
Coordinate use; never execute a shared test binary after another worktree
may have rebuilt it without first building the intended source again.

```bash
rustup run 1.97.1 rustc --version
export CARGO_TARGET_DIR=/private/tmp/plurx-streaming-reliability.1WSd2I/repo/target
rustup run 1.97.1 cargo check --workspace --locked --all-targets
rustup run 1.97.1 cargo clippy --workspace --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all -- --check
# Run the smallest production-linked regression for the change before pushing.
CARGO='rustup run 1.97.1 cargo' git commit
```

The common clone's installed `.git/hooks/pre-commit` is the tracked hook even
with `core.hooksPath` unset. It runs catalog and Rust lint plus embedded
JavaScript syntax, but no tests or effort compile. Historical anchors must be
remapped to their real renamed helpers in the code commit. A new hash-based
regression bridge belongs in a subsequent commit; a client code hash has one
bridge row, not three platform duplicates. Never use `--no-verify`.
Run `make history-check` again **after** a commit before pushing: the pre-commit
hook cannot classify its not-yet-created hash. Corrective-worded documentation
commits also need the established explicit non-runtime ledger entry (with a
truthful documentation-only reason); PR #21 initially exposed that omission.

Existing FFmpeg Full at `/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg` is 9.0.1
with libass; the default 8.1.2 lacks libass. Existing Homebrew x265 needs
`DYLD_LIBRARY_PATH=/opt/homebrew/Cellar/x265/4.2/lib` for direct test execution.
Cargo may strip it: build with `--no-run`, then run the freshly built test
binary explicitly with that environment. Do not install dependencies blindly.
Serialize iPhone 17 Pro simulator test jobs: concurrent worktrees use the same
bundle and previously terminated each other's runs.

Retained foundation logs:
`/private/tmp/plurx-astra-settlement-final-{focused,store,test-build,commit}.log`,
plus `plurx-astra-settlement-{clippy,operations,validation,web}.log`.
Final proof was 535 focused daemon tests and 27 Store contracts, not a full
qualification. The next intent worktree's fresh baseline is
`/private/tmp/plurx-astra-desired-intent-baseline.log` (passed).
Integrated capture proof is 103 Apple and 102 Android tests plus tvOS build,
web fast lane and hooks; logs are
`/private/tmp/plurx-capture-final-{apple,android,tvos-build,web}.log`.

Forgejo PR operations currently use the authenticated task-owned browser;
do not extract browser credentials. Git branch operations use the configured
SSH remote. Read actual check results, preserve the exact approved head, and
verify the resulting remote merge ancestry. Never treat a skipped release job
as qualification or an old passing run as proof for a moved candidate.
