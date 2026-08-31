# The rest of the playback-control rewrite — M5.5 through M9

**Status:** ready to plan, not all ready to build · **Executes:** items 7, 9
and 10 of
[PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md)
§6 · **Written:** 2026-08-31 · **Baseline:** `main` at `bc504681`

The third and last of three handoffs. Read whichever covers the item you are
picking up:

| item | milestone | handoff |
|---|---|---|
| 6 | M5 — one client action owner | [M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md) |
| 8 | M7 — content-analysis index | [CONTENT-ANALYSIS-INDEX-HANDOFF.md](CONTENT-ANALYSIS-INDEX-HANDOFF.md) |
| 7, 9, 10 | M5.5, M6, M8, M9 | **this file** |

## 1. Orientation — the order is not negotiable

These four milestones are strictly sequential, and each one's prerequisite is
load-bearing rather than tidy:

```
  M5   client action owner        (item 6)
   │   nothing may consume an action until one client can
   ▼
  M5.5 preparation feasibility    ── measurement, not code
   │   the acknowledgement contract is frozen FROM measured behaviour
   ▼
  M6   prepared recipe handoff    (item 7)
   │   one node must handle a transaction before three do
   ▼
  M8   cluster handoff            (item 9)
   │   the new path must be proven before the old one is deleted
   ▼
  M9   cutover and deletion       (item 10)
```

**M5.5 is a measurement milestone and skipping it is the single most expensive
mistake available here.** §5.4 requires a physical spike on all three platforms
*before* the prepared-action contract is frozen, because "ready" has to mean
something each platform can actually deliver. Freezing it from what the plan
imagines rather than what hardware does would bake a contract no client can
honour into the wire, and every later milestone builds on that wire.

**Standing rule for all of it:** if a step seems to require changing the
completion definition, stop and flag it rather than editing.

## 2. M5.5 — preparation feasibility and staged generations

Two halves that look unrelated and are not: one measures what clients can do,
the other builds the durable state that measurement gets encoded into.

### 2.1 The spike (§5.4)

Three platform-independent meanings of "ready" have to be measured, not
assumed:

| ack | means |
|---|---|
| `metadata_ready` | successor manifest/item parsed, decoder eligibility accepted |
| `buffer_ready` | successor has contiguous film time through the switch boundary **plus** its measured safety runway |
| `first_frame_ready` | observed only after commit, with wall latency and film position |

> **Asset metadata or an AVPlayer/Media3 ready state alone is not
> `buffer_ready`.** This is the sentence that decides whether the whole
> milestone works. A player reporting "ready" when it holds no successor buffer
> produces a commit that stalls at the boundary — the exact failure prepared
> handoff exists to prevent.

Each platform needs a recorded dual-preparation result **and** a named
single-player fallback:

| platform | dual proof | fallback if not |
|---|---|---|
| web/hls.js | second detached pipeline buffers without destroying current MSE/video or exceeding memory/decoder limits | server-prime then measured single-player replace |
| AVPlayer | successor item acquires playable buffer while current renders, within tvOS/iOS resource limits | `AVQueuePlayer`/single-item replace, honest interruption |
| Media3 | second player/preload manager buffers on target Android TV/phone without decoder starvation | single-player media-item replace, honest interruption |

Record: memory, decoder allocation, network duplication, old-stream continuity,
buffered-through truth, switch position error, first-frame latency — for both
same-codec and codec/grade changes.

**Unsupported dual preparation does not block the protocol.** It selects that
platform's measured fallback and *prevents the UI from calling that path
seamless*. Writing "seamless" over a path with a visible interruption is a
product lie, and §5.2 already draws the line: "a measured display-mode blink is
reported honestly rather than hidden under that term."

### 2.2 Staged generations

- `media_replacement_actions`, prepare-only activation, the one-staged-
  successor constraint, committed-pointer CAS, abort, expiry.
- **Prove prepare neither calls the legacy supersession reap nor advances
  `media_playback_pointers`.** A staged row must be invisible to ordinary
  current-session lookup until commit; if it is not, preparing a successor
  silently retires the stream the viewer is watching.

**Acceptance:** every platform has a recorded preparation result; prepare
leaves the predecessor current and running; commit advances the exact expected
pointer once; abort/expiry removes only the staged successor; owner death
during every phase leaves one durable outcome.

## 3. M6 — prepared recipe handoff (item 7)

### 3.1 The transaction

```
 current:  PLAYING ============================ RETIRING ===== X
                        |                      ^
 successor:             RESERVE -> PRIME -> READY -> COMMIT -> PLAYING
                        ^             |        |
                        |             + ACK ---+
                   action_id fences every phase
```

Eight phases in §5.1: propose · stage · reserve and prime · prepare · commit ·
commit durability · retire · abort. Three of them carry the properties worth
testing first:

- **Commit durability** is a single CAS from the expected predecessor to the
  staged incarnation. *A lost CAS aborts the staged generation; it never reaps
  a newer player generation.*
- **Abort** tears down only the successor. The current stream stays
  authoritative and playable.
- **Disconnect does not imply commit.** After the preparation lease expires the
  actor aborts the successor and keeps the current generation if healthy.

This is where the transport built in M4 and the `prior_action` fence finally
earn their keep: `prepare_replacement`, `commit_replacement` and
`abort_replacement` are the three actions that carry an `action_id`, and unlike
`hold`/`retry_resource`/`terminal` they replay exactly rather than recompute.
`resolve_action` already ranks a decided action above every advisory one.

### 3.2 One encoder slot is not make-before-break (§5.3)

The honest part of the milestone. When the only compatible hardware slot is
occupied, true make-before-break is impossible. Order of attempts: bounded
admission overcommit proven safe for that hardware → eligible software bridge →
stay on the current recipe.

A buffered break-before-make permit transfer is allowed **only** under a severe
condition and only when all four hold:

- the client reports contiguous runway exceeding measured successor startup p95
  plus a safety margin;
- current published media remains readable after its producer stops;
- the successor recipe is known eligible on the target node;
- the old recipe remains restartable at the same film-time boundary if the
  successor fails.

It is logged and measured as `buffered_break_before_make`, and **its acceptance
is bounded interruption and successful old-recipe restart, not seamless
handoff.** The finite retained buffer is not described as a running
predecessor.

### 3.3 Axis order

Resolution/bitrate first — same codec and grade, the only axis §5.2 expects to
be genuinely transparent. Then audio/subtitle burn, codec, dynamic range. Each
later axis costs a new decoder or a new encode, so each is a separate slice
with its own measurement.

**Acceptance:** scripted bandwidth cliffs and recovery satisfy §2.2; a true
make-before-break successor failure leaves the predecessor running; a one-slot
failure restarts the old recipe within its measured interruption bound;
duplicate acks/requests are harmless; physical clients record switch latency
without position regression.

## 4. M8 — cluster handoff (item 9)

Three cases, and they fail differently.

**Planned drain (§10.2).** The draining node proposes a node-replacement
action. The placer reserves the target; VOD attaches the same immutable
recipe/store where possible; rolling primes a new generation at a future film
boundary. Owner epoch/CAS prevents the drained node acting after handoff.

**Hard owner loss (§10.3).** The surviving ingress has only the absolute
snapshot in the request currently arriving — *earlier samples were relayed to
the former owner and are not assumed durable.*

- **VOD:** resurrect the immutable handle or redirect to an equivalent
  successor; segment identity is film-addressed.
- **Rolling:** **do not splice an unrelated producer into an EVENT URL** unless
  exact playlist/timeline continuity is proven. Prefer a successor session at
  an aligned boundary, returned through the current control exchange.
- **No control snapshot:** fall back to persisted watch position and fetched
  frontier, then require a normal reopen. *Do not guess transparency.*

**Split-brain (§10.4).** Only the owner with the current replicated
`owner_epoch` may issue a mutating action.

The existing typeless-sliding same-session takeover stays as a compatibility
path until successor-generation failover is complete, and **must not be
expanded to VOD or EVENT sessions**.

**Acceptance:** three-node fault tests cover drain, hard kill during each
phase, duplicate commit, stale owner, and shared-store loss; one committed
owner and one client-visible transaction result in every case.

## 5. M9 — cutover and deletion (item 10)

Only after mixed-fleet evidence:

- default protocol advertisement on;
- default the actor recovery engine on, remove the compatibility engine;
- remove `/status` polling and deprecated client recovery code;
- re-evaluate whether `live-hls-recovery` is still necessary once VOD
  eligibility covers the supported catalog.

**Acceptance:** a repository search finds only the three approved progress
deadlines and the named lifecycle timers; playback-lab plus physical matrices
green; **rollback uses the previous release, not hidden dead code.**

That last clause is a constraint on how you delete: leaving a disabled copy of
the old engine "just in case" fails this milestone. The rollback story is
shipping the previous release.

## 6. What must be true before any of this starts

Repeated from the M5 handoff because it gates every milestone here too:

```
plurx_playback_control_vocabulary_total{complete="true",platform="…"}
```

reads **zero** as of 2026-08-31. The fleet has never run a build that can
receive a control action. Apple 95 and Android 54 exist and have not been on
hardware; the last recorded fleet mobile release was Android 47 · Apple 86.

Every milestone in this file assumes clients that act on actions. None of that
is observable until the fleet runs one.

## 7. Working rules

§6 of [M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md)
lists the CI gates that fail late — `cargo fmt`, `-D dead-code`, the mobile
build gate, `history-check`, the ownership ledger — and the two mistakes that
cost the 2026-08-30 session most. All of it applies here unchanged.

Two more that bite specifically in this half of the roadmap:

- **`crates/plurxd/src/playback_control.rs` is ~16k lines and holds the actor.**
  M6 and M8 both land in it. Two agents in that file conflict on ideas, not
  just on text; sequence them rather than parallelising.
- **Cluster contract jobs take 15+ minutes on shared self-hosted runners** and
  are the real pacing constraint for M8, whose acceptance is *three-node fault
  tests*. Budget CI time as a first-class cost, not an afterthought.

## 8. Open questions

Carried from the M5 handoff and still unanswered, because they shape M6 rather
than M5:

1. How long may a client wait for an action before falling back to its own
   behaviour?
2. Does `terminal` end playback outright, or offer the verdict with a *Try
   again*?
3. Does a retry bound belong on the client at all?

New here:

4. **Is a bounded admission overcommit proven safe on any of the fleet's
   hardware?** §5.3's first fallback assumes one exists. If none does, the
   one-slot path is the common case rather than the exception, and M6's shape
   changes accordingly.
5. **What interruption bound is acceptable** for `buffered_break_before_make`?
   Its acceptance criterion is "within its measured interruption bound", and
   nobody has measured or chosen one.
