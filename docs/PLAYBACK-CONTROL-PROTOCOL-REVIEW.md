# Playback control protocol review — twelve contracts corrected before code

**Status:** adversarial review complete · all blocking/high findings accepted
and reconciled in
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) ·
reviewed 2026-08-26 against draft commit `eed82b9a` and merged `main`
`64d8f4de`

An independent adversarial agent reviewed the plan against the merged server,
cluster store, live/VOD delivery engines, and all three clients. The review was
asked to find reasons the design could not be built or could fail under replay,
takeover, resource pressure, or mixed-version rollout. This record preserves
the findings and the disposition; it is not a second implementation plan.

## 1. Blocking findings

### 1.1 Sequence fencing did not survive owner takeover

The draft kept `session_id` stable through takeover, stored sequence only on
the owner, and gave the successor no value against which to reject a captured
pre-takeover `hold`, selection, acknowledgement, or `end`.

**Accepted correction:** control now carries non-secret `incarnation_id` as
generation, durable `owner_epoch` as `control_epoch`, a client-instance UUID,
and a sequence scoped to that tuple. An old epoch receives
`409 owner_changed` before mutation or renewal. See plan §§1.4, 3.2–3.5,
8.1, and 10.1.

### 1.2 Equal-sequence replay could retain an encoder forever

The draft let idempotent replays renew. A stuck client or captured request
could therefore keep a session alive without fresh state, and a cached
`expires_in_ms` could not remain both byte-identical and truthful.

**Accepted correction:** only a newly accepted higher sequence renews. Equal
sequence returns equivalent action outcome without renewal. Lease responses
carry absolute expiry plus server time. See plan §§3.3–3.4 and 8.1.

### 1.3 Today's create path cannot prepare a successor

Rolling creates call the supersession reap, and durable activation advances
the playback pointer/ends the predecessor. Using that path for preparation
would destroy the stream the transaction promises to keep.

**Accepted correction:** prepare-only activation and durable replacement
staging move before prepared handoff. Prepare does not reap or advance
`media_playback_pointers`; commit CASes the expected pointer; abort removes
only the staged generation. See plan §§5.1, 11.1, and milestone M5.5.

### 1.4 The one-slot rollback claim was impossible

Once the only producer is stopped and its permit transferred, a failed
successor cannot leave that producer “running.” Retained bytes are finite.

**Accepted correction:** the plan distinguishes true make-before-break from
bounded hardware overcommit, a software bridge, staying put, and an explicitly
interruptible `buffered_break_before_make`. The last path attempts an
idempotent old-recipe restart and has its own interruption acceptance. See
plan §5.3 and M6.

### 1.5 Semantic annotations could not live in `FragmentIndex`

The existing fragment index is a packed, node-local structural sidecar. Its
store would not replicate a new Serde annotation field, while current clients
already consume the additive `DecisionResponse.markers` contract.

**Accepted correction:** the operator-visible content-analysis index has
separate node-local fragment rows, replicated timeline annotations, and
optional node-local detector features. Existing marker fields, including
`chapter`, remain during migration. See plan §6.5.

### 1.6 The analysis queue lacked a durable contract

The draft named endpoints and UI states without job identity, attempt fencing,
claim CAS, retries, cancellation, atomic publication, or the distinction
between cluster-once semantic work and per-node structural work.

**Accepted correction:** the plan now defines job, attempt, artifact, and
node-coverage records; legal transitions; claim epochs; force deduplication;
typed retry/cancel/stale behavior; staged publication CAS; and node-local
high-frequency progress aggregated through Activity. See plan §6.6.

## 2. High-risk findings

### 2.1 Control relay is a mutation, not a media read

The current relay resource has no body or expected owner epoch, and its peer
authorization treats only delete as a write. Adding control there would let a
read-authorized route renew, end, acknowledge, or change selection.

**Accepted correction:** control uses a separate bounded mutating relay
envelope with exact write authorization and
`(incarnation_id, owner_node_id, owner_epoch)` fencing on ingress and owner.
See plan §10.1 and M1.

### 2.2 Current expired-route handling cannot coordinate takeover

The current HLS relay returns gone for an expired route. It has no
`owner_transition` state, and an ingress does not retain every sample relayed
to the old owner.

**Accepted correction:** ingress control resolution now distinguishes active
local, active remote, takeover settling, expired claimable, and terminal. Hard
recovery uses the absolute snapshot in the current request; it does not assume
older state is durable. See plan §§10.1 and 10.3.

### 2.3 M1 claimed a lease it did not implement

The draft described M1 as a no-op touch but accepted a new active lease even
though rolling, VOD, and cluster owner lifetimes are currently 60, 300, and 12
seconds respectively.

**Accepted correction:** M1 adds real `ControlState` to both HLS engines,
reports the selected engine's actual legacy lifetime, and does not advertise
the provisional 30-second actor lease before M3. See plan §8.1 and M1.

### 2.4 Client readiness was not portable or proven

Metadata readiness is not buffered successor media. Dual AVPlayer, Media3,
and web/MSE preparation can consume a second decoder and substantial memory,
and current clients own one active player.

**Accepted correction:** readiness is split into `metadata_ready`,
`buffer_ready` with film-time coverage, and post-commit `first_frame_ready`.
A physical feasibility gate measures dual preparation and defines a
single-player fallback per platform before M6. See plan §5.4 and M5.5.

### 2.5 The existing indexer cannot emit audio/visual detector evidence

It is video-only structural indexing; it disables audio/subtitles and does not
decode frames or OCR. Treating semantic detection as free output from that
pass was inaccurate.

**Accepted correction:** authored/manual markers ship independently. Optional
audio/video/visual features use a separately budgeted sidecar job and remain
opt-in until a labeled evaluation records precision/recall. See plan §6.5 and
M7.

### 2.6 The watchdog inventory was not exact enough

The draft grouped client recovery into one row even though Apple, web, and
Android currently have distinct progress, delivery, black-frame, initial-wait,
error, reopen, and budget owners. It also overstated what control can promise
while an OS fully suspends a client.

**Accepted correction:** plan §7.4 now maps current source symbols and their
mutation/disposition per client. Exactly three future playback progress
deadlines remain in §7.1. Ordinary subtitle/UI/debounce timers are classified
separately. Section 8.1 scopes indefinite pause to delivered heartbeats and
defines bounded dormant background behavior.

## 3. What the review found sound

The reviewer endorsed these foundations:

- one actor owns delivery mutation and one arbiter serializes actions;
- client state is an absolute snapshot rather than inferred deltas;
- VOD remains first and the live engine remains a typed prerequisite fallback;
- progress watchdogs are distinguished from lifecycle/HTTP/admission timers;
- “transparent” means transactional and position-preserving, not a false
  promise that decoder/display reconfiguration is invisible;
- rebuilds publish atomically and leave the current artifact serving;
- instrumentation uses bounded, low-cardinality state.

## 4. Review gate result

The first draft was not implementation-ready. The reconciled plan is ready for
M1 because M1 now includes the identity, replay, lifetime, and mutating-relay
fences it claims. Prepared handoff remains gated on M5.5's durable staging and
physical client feasibility. Automatic semantic detection remains gated on
M7's separate feature pipeline and evaluation evidence.
