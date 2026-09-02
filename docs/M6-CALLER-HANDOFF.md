# M6's caller — where the preparation decision is made, and by whom

**Status:** ready to build · **Executes:** the remainder of
[REMAINING-ROADMAP-HANDOFF.md](REMAINING-ROADMAP-HANDOFF.md) §3, after the
slot, the executor and the decision ·
**Written:** 2026-09-02 · **Baseline:** `main` after
[#796](https://github.com/pjunod/plurx/pull/796)

Companion to [M6-IMPLEMENTATION-HANDOFF.md](M6-IMPLEMENTATION-HANDOFF.md)
(what M5.5 measured and what M6 must do about it) — this is *the one design
question that document left open*, answered, plus the slices that follow from
the answer.

**Standing instruction.** §2 is a decision, not a proposal. If a step seems to
require moving the decision back into the actor, stop and say which step — the
reason it lives where it does is in §2.3, and the alternative was costed.

---

## 1. What exists, and the gap

| piece | PR | state |
|---|---|---|
| the actor's preparation slot | [#792](https://github.com/pjunod/plurx/pull/792) | merged |
| the executor — actor gate, store CAS, actor told | [#793](https://github.com/pjunod/plurx/pull/793) | merged |
| the three durable outcomes, pinned | [#796](https://github.com/pjunod/plurx/pull/796) | merged |
| `decide_preparation` — prepare or fall back | [#798](https://github.com/pjunod/plurx/pull/798) | open |

The server can hold, commit and abort a staged successor, and it knows when it
should. **It cannot notice that a viewer asked for one.** Nothing calls
`decide_preparation`, and `PreparationExecutor` still carries
`#[cfg_attr(not(test), allow(dead_code))]`.

### 1.1 The four inputs, and where each already lives

`decide_preparation(delivered, candidate, retained, conditions)`:

| input | where it lives today |
|---|---|
| `delivered: &EffectiveSelection` | `DeliveryView::effective_selection`, actor-side |
| `retained: Option<&DynamicCapabilities>` | `ControlRequestV1::capabilities`, **sequence 1 only** — retained by nothing |
| `conditions.observed_download_bps` | `ControlRequestV1`, every exchange |
| `conditions.delivered_bps` | `DeliveryView`, actor-side |
| `candidate: &EffectiveSelection` | **nowhere** — see §1.2 |

Three of the four converge in one place already: the control exchange
(`hls::control_local_inner`) holds the request and builds its response from the
delivery view. Only the candidate is missing, and one of the three needs
retention.

### 1.2 The candidate does not exist, and cannot be read off the wire

The client sends `selection: ClientSelection` every exchange — *intent*:
`QualitySelection::Auto` or `Manual { height }`, `CodecPolicy::Auto`,
`DynamicRangePolicy::Auto`. `EffectiveSelection` is the *delivered output*: a
concrete height, and `codec` as one of exactly two values, `source` or
`server_selected`.

They are not the same kind of thing and cannot be compared. A client on Auto
being delivered 1080p has not "requested 1080p", and a client that switches
from Auto to Manual 1080 may be asking for the same rung it is already on or a
different one — only the ladder knows.

Resolving intent into a candidate is the work `hls::create` already does:
`review_client_plan` against the source file and the render caps, then the
ladder, then `EffectiveSelection::from_recipe`. **That path is not callable
from anywhere else**, and making it callable is §3.2.

## 2. The decision: the exchange decides, the actor still fences

### 2.1 What is decided

The control exchange resolves the candidate, calls `decide_preparation`, and on
`Prepare` stages through `PreparationExecutor`. The actor keeps the slot and
keeps the commit gate; it does not evaluate the axis policy.

### 2.2 Why not the actor

The actor is the ordering authority and the slot lives there deliberately — a
preparation committable from outside that ordering would be a second authority
over one playback, which is what
[`PreparationSlot`](../crates/plurxd/src/playback_control.rs)'s own doc says.
Putting the *policy* there too would mean the actor calling into plan
resolution, which needs the store (the source file), the render caps and the
ladder. That is a dependency the actor does not have and should not acquire: it
is a bounded mailbox with a lease clock, and every store read inside it is a
new way for an exchange to miss its deadline.

The alternative shape — the actor asks the session layer to resolve a
candidate and waits — adds a round trip inside the exchange's own deadline to
compute an answer the exchange could have computed before it started.

### 2.3 Why the invariant survives

The thing that must not move is **who may take the slot and who may commit**,
and neither moves. `stage_preparation` still refuses an occupied slot and a
terminal playback; `may_commit_preparation` still refuses a successor the actor
has forgotten; `terminate` still aborts the slot it is holding. The exchange
proposes; the actor disposes. That is the same relationship producer decisions
already have, and M7's plan warns against inventing a third mechanism — this
invents none.

**What this does mean:** the decision is computed from a snapshot that is one
instant old by the time the actor sees the stage request. That is already true
of every input the exchange reads, and the slot is what makes it safe: a
transition the actor no longer wants is refused at `stage_preparation`, not
prevented by having decided later.

## 3. The slices, in order

Each is independently mergeable and each has an acceptance that is a test or an
observable fact, not a feeling.

### 3.1 The actor retains the capability document

**Why first:** it is a latent defect today, independent of everything else.
`ControlRequestV1::validate` requires `capabilities` only on sequence 1 and
permits every later exchange to omit them. Any consumer reading this exchange's
`capabilities` sees `None` for the whole session after the first message —
which for a capability meaning *this device can hold two live pipelines* would
refuse every transition a capable client ever makes.

Retain the sequence-1 document on the actor; let a later exchange that sends
one replace it. A client that changes its answer mid-session is telling the
truth about a device that changed (a TV that woke a second decoder, a phone
that lost one), so last-write-wins is right and no reconciliation is owed.

**Acceptance:** a unit test in which sequence 1 carries
`dual_player_preparation: true`, sequences 2 and 3 omit `capabilities`
entirely, and the retained document still reads `true` at sequence 3.

### 3.2 Candidate resolution becomes callable

Extract the plan → recipe → `EffectiveSelection` path out of `hls::create` so
the exchange can ask *"if this client's selection were honoured right now, what
would we deliver?"* without creating anything.

**Do not duplicate it.** Two resolvers that agree today and drift tomorrow is
how a prepared successor ends up being a recipe the create path would never
have produced — and the drift is invisible, because both sides look correct in
isolation. `create` must call the extracted function too.

**Acceptance:** a test asserting the extracted resolver and `create` produce
the same `EffectiveSelection` for the same body and source file.

#### 3.2.1 The extraction map

Verified against `hls.rs` at `main` after
[#800](https://github.com/pjunod/plurx/pull/800). Re-verify at build time in
case it moved — but the shape is unlikely to, because each piece is already a
named function.

The resolver is the span of `create` from `hdr10_requested` (`hls.rs:1222`) to
`apply_plan_review` (`:1300`). It does four things, in order, and only the
second is inline:

| step | today | callable? |
|---|---|---|
| 1. review the client's plan against its caps and the file | `review_client_plan` | yes |
| 2. resolve the height — Auto answered, explicit rungs snapped, the source-height promise honoured | inline, `:1245-1261` | **no** — this is the extraction |
| 3. build the request | `CreateSession::into_request(file_id, height)` | yes, and already unit-tested |
| 4. apply the review's DV/HDR10 decisions to it | `apply_plan_review` | yes |

Step 2's three arms are policy, not arithmetic, and the comments say why:
Auto's answer is *not* snapped because snapping would re-decide policy (a 900p
source deliberately transcodes at 900, with no scaler in the chain); the
source's own height is the Original/forced-burn promise and is never snapped or
downgraded; anything else is an explicit rung from a menu and strays snap onto
the ladder while above-ladder heights pass through. Extract it whole. Splitting
it is how one of those three promises gets lost.

Its inputs are `req.height`, `source_height`, the ladder ceiling
(`capability_height_ceiling_for_request`), the stored network prior, and
`hdr10_requested`. The network prior needs the request's identity, which the
control exchange has: it is the same session.

**The delivered grade is _not_ predictable, and the first version of this
section said it was.** Corrected 2026-09-02, before anything was built on it.

`session_delivered_dynamic_range(source, kind, grade)` (`hls.rs:1024`) *is* a
pure function of its three arguments — that much was right. The error was in
the third: `grade` comes from `Pipeline::output_grade()`
(`plurx-core/src/transcode/pipeline.rs:430`), and the pipeline is chosen from
what the node proved and what the session is doing, at start. `create` reads it
off the built session and its own comment says why:

> The grade the session actually built, not the one the body asked for: the
> server refuses the HDR10 rung for a source or a rung that cannot prove it,
> and the badge has to follow the encoder.

So a candidate that will never be built has no delivered grade, and the resolver
cannot return one.

**Do not paper over it by predicting.** Two shapes were considered and both are
wrong:

- *Abstain* — give the candidate `dynamic_range: None` and let
  `decide_preparation`'s unknown-abstains rule handle it. This fails in the
  unsafe direction: a transition that really is a grade change classifies as
  resolution-only and gets **prepared**, which is the one outcome
  `PREPARED_AXIS` exists to prevent.
- *Predict optimistically* — give the candidate the grade its body asked for
  after review. This fails in the safe direction (a spurious fallback) but
  fails constantly: a session whose HDR10 rung the encoder refused reads
  `delivered = sdr` against `candidate = hdr10` on every exchange, so a viewer
  on that session never gets a prepared handoff for a plain quality change.
  Both are comparing an encoder's answer against a request, which is comparing
  unlike things.

**Compare like with like instead.** The grade axis has to be read off the
*request* on both sides, not off one request and one encoder. The exchange
already holds the session's own `RemoteStartRequest` — `local_control_response`
takes it as `recipe` — so both the delivered session's intent and the
candidate's are available, and comparing them is a comparison of two of the same
kind of thing.

That is a change to what `decide_preparation` takes, not just to what the
resolver returns, and it is the first thing slice 3.2 has to settle. Until it
is settled, **do not construct a candidate `EffectiveSelection` at all** — one
carrying a grade nobody can justify is worse than no candidate, because the
decision it feeds looks correct.

The rest of this section stands: the height resolution is the extraction, and
the other three steps are already named functions.

**Suggested shape**, so `create` and the exchange cannot drift:

```rust
struct ResolvedPlan {
    request: crate::transcode::SessionRequest,
    height: i64,
    delivered_dynamic_range: Option<String>,
    plan_notes: Vec<PlanNote>,
}

async fn resolve_plan(
    state: &AppState,
    source: Option<&MediaFile>,
    body: CreateSession,
    network_prior: Option<&NetworkPrior>,
) -> Result<ResolvedPlan, ApiError>
```

`create` calls it and keeps everything after `:1300` — the fingerprint, the
validity checks, admission, activation. The exchange calls it and throws the
request away, keeping only the `EffectiveSelection` it can build from it.

**The one thing to get right:** the fingerprint at `:1294` is taken from the
request *before* the review is applied, deliberately, so a transport retry that
lands on a different binary fingerprints identically. `resolve_plan` returning
a post-review request would move that line's meaning. Either return both, or
leave the fingerprint where it is and have `create` take it from the
pre-review request — the comment at `:1291-1293` is the specification.

### 3.3 Shadow mode — decide, record, change nothing

Call `decide_preparation` in the exchange and emit the outcome as a metric.
Stage nothing.

**Settle this first: there is no mapping from a client's selection to a create
body.** `resolve_plan` (slice 3.2, merged) takes a `CreateSession`. The
exchange has a `ClientSelection` — `QualitySelection::Auto | Manual { height }`,
`CodecPolicy`, `DynamicRangePolicy`, an audio track, an offset, a subtitle mode
— and the two are different vocabularies, not two spellings of one.

The base is not in doubt: the exchange already holds the session's own
`RemoteStartRequest` as `recipe`, so the candidate is *that body with the
client's selection applied*, not a body built from nothing. What is in doubt is
each field:

- **`QualitySelection::Manual { height }` → `CreateSession::height`** is the
  easy one, and `Auto` → `None` with `quality_auto: Some(true)`. Note that
  `into_request`'s `automatic` falls back to *wire presence* when
  `quality_auto` is absent, and a subtitle burn sends the source height as a
  promise rather than as a quality answer — so the candidate must set
  `quality_auto` explicitly rather than let presence infer it.
- **`CodecPolicy` and `DynamicRangePolicy` are client *policies*, not the
  server's `copy` / `hdr10` / `preserve_dolby_vision` answers.** `create`
  derives those from the caps document through `review_client_plan`, which the
  selection does not carry. The honest reading is that a selection change on
  those axes means *re-review*, and until that is settled a candidate should
  carry the session's existing answers rather than invent new ones — which also
  means a codec or grade selection change is not yet expressible as a
  candidate at all. `decide_preparation` refuses both axes anyway
  (`AxisNotProven`), so nothing is lost today; it will matter the moment the
  capability is narrowed.
- **`SubtitleSelection` is not `subtitle_burn`.** `Off`/`Native`/`Overlay` are
  not burns; only `Burn` is. Mapping `track` into `subtitle_burn`
  unconditionally would turn every native-subtitle change into a burn — a whole
  new encode recipe — and `decide_preparation` would correctly refuse it, for
  the wrong reason.

**And a `CreateSession` may be the wrong shape for a candidate entirely.**
`into_request` hardcodes `convert_dolby_vision: false` and says why: *"a client
cannot ask to be handed a conversion — whether one happens is decided from its
caps and the node's, and create overwrites this from the plan it re-derives."*
Only `apply_plan_review` ever sets it.

So a candidate built as a `CreateSession` and resolved **without** a review can
never carry `convert_dolby_vision: true`. For a session that *is* converting
Profile 7 to 8.1, the candidate's `GradeIntent` then differs from the delivered
one on that field alone — and `decide_preparation` reports a `dynamic_range`
crossing on **every exchange, forever**, for a viewer who changed nothing. That
is precisely the confident wrong measurement this section exists to prevent, and
it would look entirely plausible in the metric: DV titles simply never prepare.

Two ways out, and the choice is the slice's first decision:

1. **Build the candidate as a `SessionRequest`, not a `CreateSession.`** The
   session already has one; a selection change edits the fields it names and
   leaves the rest — including `convert_dolby_vision` — alone. Then
   `resolve_plan`'s job shrinks to the height, which is the only part a
   selection cannot answer for itself. Truthful by construction, but it means
   the exchange no longer shares the whole of `create`'s path, which is the
   drift risk §3.2 was written to avoid.
2. **Keep `CreateSession` and carry the session's plan answers alongside it**,
   applying them the way `apply_plan_review` does. Shares the path, but
   re-introduces exactly the "derive from what you were given, do not accept it
   alongside" seam that #809's review closed on `hdr10_requested` — so it needs
   the answers to come from the session's own request rather than from a
   caller's argument.

**Ruled: build the candidate as a `SessionRequest`** (option 1). Decided
2026-09-02 under Paul's standing authority to decide when he is not here, and
recorded rather than left open.

The argument that looked like a cost — *the exchange stops sharing the whole of
`create`'s path* — does not survive inspection. Ask what sharing actually buys.
`resolve_plan` does four things, and only one of them is knowledge a candidate
lacks:

| step | does a candidate need it? |
|---|---|
| `review_client_plan` | **no** — a candidate carries the session's existing plan answers; a selection does not re-review |
| the height resolution | **yes**, and only this — it needs the store, the ladder ceiling and the network prior |
| `into_request` | **no** — it turns a *wire body* into a request, and a candidate does not come from the wire |
| `apply_plan_review` | **no** — nothing to apply |

So the shared surface worth protecting is the height resolution, and it is
protectable on its own: lift it out of `resolve_plan` as `resolve_height` and
have both callers use it. Everything `into_request` would contribute is
information the candidate already has more accurately, in the request the
session is actually running.

Option 2 buys the appearance of a shared path and pays for it by round-tripping
through a type that provably loses a field — and then needs the lost field
handed back alongside, which is the seam #809's review closed. A shape that has
to be repaired at every call site is the wrong shape.

**What this makes the next slice.** Not "map a selection to a create body", but:

1. lift `resolve_height` out of `resolve_plan`, both callers using it;
2. `fn candidate_request(current: &SessionRequest, selection: &ClientSelection,
   height: i64) -> SessionRequest` — edit the fields the selection names, leave
   the rest;
3. the test below.

**Expect (2) to be the hard part, and expect it to be policy rather than
mapping.** A quality change on a `Copy` session is the case to think about
first: a copy has no height, so honouring a rung means becoming a transcode —
which is a `DeliveryMethod` crossing and refused anyway, but the candidate has
to *say* so rather than silently keep copying. Each branch like that is a
decision; write them down as they are made, the way this file writes down the
ones before it.

Whichever is built, the test that proves it is the same: **a converting
Profile 7 session, with the client changing nothing, must read `Unchanged`.**
Write that test first; it fails on both the obvious implementations.

Write the mapping down as a function with its own tests before wiring the
metric. A shadow mode fed a wrong candidate produces a *confident* wrong
measurement, and the whole point of the slice is that the measurement is
trustworthy enough to act on.

**Done**, as [#816](https://github.com/pjunod/plurx/pull/816):
`candidate_request` and `EffectiveSelection::from_request`, with the converting
Profile 7 property pinned. What remains is the wiring — and it has a cost worth
knowing before it is written.

#### 3.3.1 Building a candidate costs two store reads

`resolve_height` needs the source file and the network prior. Both are store
reads, and the control exchange runs about once a second per client under an
absolute deadline. Doing them unconditionally, on every exchange, to answer a
question that is almost always "nothing changed", is the wrong shape.

**Gate them behind a cheap comparison.** Everything a selection names —
quality, audio track, audio offset, subtitle mode and track — is already on the
snapshot, and the delivered values are already on the delivery view. Compare
those first; resolve and build a candidate only when one of them moved. The
expensive path then runs at the rate viewers change something, which is orders
of magnitude below the exchange rate.

This works because of what the comparison is actually looking at, and that is
worth stating plainly:

**M6 prepares for a change the *client* asked for.** A selection is the
viewer's intent, and a transition exists when that intent stops matching what
is being delivered. A server-driven Auto rung change — the adaptive ladder
moving because the link moved — is *not* a selection change and this path never
sees one. That is plan §6.1's territory, not M6's, and conflating them is how a
prepared handoff would start firing on exactly the congestion the throughput
gate exists to refuse.

The throughput gate still earns its place: a viewer who pins 2160p on a bad
link is a client-driven change that doubles demand at the worst moment. But the
gate is protecting against a viewer's choice, not against the server's own
adaptation — and if a future slice does bring adaptive rung changes into this
path, that is a new decision and not an extension of this one.

**Why this slice exists at all:** the axis restriction in `PREPARED_AXIS` is an
argument. Shadow mode turns it into a measurement — how many transitions would
prepare, how many fall back and on which axis, and how often
`throughput_unproven` fires, which is the residual most likely to make the
whole path never fire in practice.

Suggested shape, one counter with two labels rather than a family:

```
plurx_playback_preparation_decisions_total{decision="prepare|fallback",axis="…",reason="…"}
```

The vocabulary is already fixed and tested: `PreparationAxis::as_str` and
`FallbackReason::as_str`.

**Acceptance:** the counter is nonzero on a node serving real traffic, and its
`axis` and `reason` distribution is recorded in the status page with a date.
Read it as "this build has decided", captured at the time — these are in-memory
counters and every deploy resets them (M6 handoff §4).

### 3.4 Stage on `Prepare`

The first behaviour change, and the first production caller — this is the slice
that removes `allow(dead_code)` from `PreparationExecutor`.

**It fires on nothing today**, by design: all three clients hardcode
`dual_player_preparation: false`, so every decision is
`fallback/client_cannot_prepare` until a coordinated client release flips
Apple's literal. That is handoff §1 working, not a bug — but it does mean §3.3's
measurement is what tells you the plumbing is right, because §3.4 cannot be
observed on the fleet until the release ships.

**Acceptance:** with a test client reporting `dual_player_preparation: true` on
a resolution change with headroom, the ledger holds a staged successor and the
pointer still names the predecessor.

### 3.5 The commit trigger

The client acknowledges readiness and the exchange commits. **Blocked**, and
not on code: plan §13.6 freezes the three acknowledgements from what M5.5
measures, and `first_frame_ready` is the ack whose Apple instrument was the one
in question — [M5.5-APPLE-NOT-ACCEPTED.md](M5.5-APPLE-NOT-ACCEPTED.md) §2.2.
Freezing an ack against an instrument that cannot separate a codec change from
no change would bake the defect into the protocol.

Until then the executor's `commit` has tests and no trigger, which is the
honest state.

## 4. Non-goals

- **Do not move the slot or the commit gate out of the actor.** §2.3.
- **Do not re-derive `dual_player_preparation`.** Read the retained field.
  Three literals were written by assumption once already.
- **Do not widen `PREPARED_AXIS` without narrowing the capability first.** Its
  own doc carries the reason: the Google TV's only hard failure was on
  same-codec, so the restriction is not a safe default for the television
  class — it is a decision with a live residual.
- **Do not call a prepared path seamless** on any platform whose fallback
  interruption is measured. Plan §5.2, and M5.5 gave it numbers.
- **Do not describe a finite retained buffer as a running predecessor**
  (roadmap §3.2).

## 5. What is still owed from M5.5, and will surface here

**Apple's fallback is unmeasured**, because Apple passed dual preparation and
never exercised the fallback. If §3.3's shadow mode shows Apple transitions
falling back — and it will, until the literal flips — the interruption those
viewers take is a number nobody has.

**Nobody has twenty consecutive commits on a realistic runway.** M5.5's 20/20
ran on a fixture short enough to buffer completely; the corrected instruments
ran four trials per device on a 132-second fixture. The two halves were
measured under different conditions, and §3.4's acceptance should say so
rather than inherit it silently.
