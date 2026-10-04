# Continuous quality — build uninterrupted resolution changes

**Status:** production implementation in progress; upstream integrated;
final adversarial review and qualification pending · **Written:** 2026-09-30 ·
**Source anchor:** `origin/main` at `1b2ae4f62e7d131d18c088a643470e40cdb9789c`
· **Effort:** `effort/continuous-quality`

This is the implementation contract for Paul's request to remove the pause,
flash and audio seam during quality changes on every client, starting with
the web. Read §2–§5 before building, execute §9 in order, and keep the evidence
in §10. The September 16
[continuity build](QUALITY-SWITCH-CONTINUITY-BUILD.md) explains how prepared
replacements became reachable; this document supersedes its mandatory
reopen-on-failure policy, its Apple item-swap constraint, and its exclusion
of multivariant HLS **for this effort**. Existing production behavior remains
as implemented until the corresponding milestone lands.

Companions: [adaptive quality](../streaming/ADAPTIVE-QUALITY.md),
[lifecycle coverage](PLAYBACK-LIFECYCLE-COVERAGE.md),
[web asset layout](../clients/WEB-SHELL-LAYOUT.md),
[development pipeline](../DEVELOPMENT_PIPELINE.md), and the
[adversarial review](CONTINUOUS-QUALITY-REVIEW.md). Re-verify source symbols
at build time; the source has moved since the older continuity documents.

## 1. Outcome — changing quality must not restart healthy playback

The viewer can select a resolution or remain in Auto while the film keeps
moving and audio keeps playing. The requested quality can take time to
arrive. A failed optional change leaves healthy playback running and gives
a truthful, nonblocking result. The quality shown as delivered changes only
when that rendition is actually presented.

There are two delivery mechanisms:

1. **Continuous rendition switching** within a compatible HLS presentation.
   Keep the player, media attachment, audio rendition and film timeline;
   switch video segments at an independently decodable boundary.
2. **Prepared pipeline replacement** when the delivery family changes, such
   as source-copy to transcode or a codec/HDR/audio change. Keep the current
   picture until the destination has real presentation evidence. This remains
   a separately measured path and earns no automatic claim of continuity.

The order is deliberate: instrument and repair existing web behavior first,
then build continuous HLS, then adopt it on the native clients. Completing
web work does not close the native acceptance rows.

### 1.1 Scope and guardrails

| In scope | Boundary and reason |
|---|---|
| VOD manual quality and Auto rung changes | The reported issue; preserve existing selection and recovery authority. |
| Web hls.js, native Safari HLS, iOS, tvOS, Android phone and TV | Each engine requires its own evidence. Playwright WebKit is not physical Safari evidence. |
| Compatible bitrate/resolution changes | Start with H.264 SDR and shared AAC audio; extend to compatible HEVC/HDR families only with the same tests. |
| Existing prepared path | Repair failure policy and presentation ownership across all clients. |
| Server rendition ownership, admission, cancellation and telemetry | A playlist alone does not supply an on-demand encoder. |
| Live TV, DRM, offline downloads, audio-track changes, burn-track changes | Preserve behavior and run regressions; do not convert them into the first continuous ladder. |
| HDMI output-mode changes | Measure separately. Codec/HDR/frame-rate output changes can require a display renegotiation; no promise that a software fade removes it. |
| Native Auto policy rewrite | Out of scope. Integrate with the existing policy and the [native quality design](../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md), rather than replacing its authority accidentally. |

No CSS fade, still-frame cover, zero dropped-frame counter or successful HTTP
response is evidence that moving pictures and audio continued.

### 1.2 Concurrent work — consume display-aware Auto, do not rebuild it

Paul explicitly required non-overlap with **Check Cinema transcoding
resolutions**, session `01a0f484-4b71-76d2-acfe-0b0975249d9c`. Its live work
was inspected on 2026-09-30 in the independent clone
`/private/tmp/plurx-auto-quality`, branch `codex/display-aware-auto-quality`,
HEAD `424f7d1625ddab2556cb18e84579164c08008f0b`, with additional uncommitted
implementation. That source is a moving dependency, not a branch to edit or
cherry-pick selectively. Its `DISPLAY-AWARE-AUTO-QUALITY-PLAN.md` §4.2 and
§7 own voluntary handoff safety, admission, candidate routing and native Auto.
Its latest updates explicitly describe failed-upgrade retention and spare-slot
admission fixes in progress. No message was sent to that session and no claim
is made that its owner agreed to transfer work.

| Responsibility | Owner / rule for this effort |
|---|---|
| Display fit, 1440/intermediate rungs, candidate identities, recipe/rate and manifest bandwidth correctness | Existing display-aware Auto session. Consume its resolved candidates; do not create another ladder or redo those fixes. |
| Auto reducers, transfer telemetry, original restoration, cooldowns, voluntary-upgrade retain-current and spare-capacity admission | Existing session. CQ1 verifies its landing and adds only any still-missing manual-change/continuous-media behavior. |
| Strict parser floor, route negotiation, target/capability revisions and existing prepared ownership | Existing session. CQ2a first reuses its landed contracts; extend only the independently missing cancellation/rendition semantics. No competing discovery/negotiation stack. |
| Same-player video-segment changes, shared audio, append provenance, bounded rendition serving, presentation capture | This effort's new scope. Production integration waits for the dependency boundary below. |
| Existing player, server, policy and fixture files edited by both plans | Serialize after the existing session lands and releases those surfaces. An isolated branch prevents accidental edits, but does not prevent duplicated work. |

**Immediate build allowance:** CQ0 may start now in this effort's independent
clone. Own new investigative files only: `scripts/continuous-quality-lab.mjs`
and `tests/playback/continuous-quality/`, plus this build/review record.
The user-authorized allowance extension on 2026-09-30 also permits the exact
additive path `scripts/continuous-quality-lab.mjs` under the existing
`playback.pipeline` owner in `validation/points.toml`, and the requested
indexed status page. Preserve every unrelated catalog entry. Use
read-only copies of the vendored library and generated media under this
clone's `target/continuous-quality/`. Do not edit the shared playback lab,
existing fixtures, production Rust/JS/Swift/Kotlin, settings, or the other
session's checkout during this phase. The new lab is a disposable/integrable
feasibility probe, not a second production acceptance oracle; its captures
must later be wired into the existing playback lab.

**Dependency boundary before production edits:** read that session's current
status and source diff again, identify its completed landing commit(s), fetch
authoritative main, and record the integrated base and ownership map in §10.
The upstream implementation must be landed and no longer actively editing
our shared surfaces, or the user must explicitly reassign a concrete scope.
Do not infer completion from a green compile or a clean checkout. Reconcile
CQ1/CQ2a against the landed behavior, remove duplicate tasks, and obtain an
adversarial delta review of that integration before CQ2a or other production
work begins. This is dependency serialization, not a feature enable gate.

If the dependency is still active after CQ0, finish the independently owned
prototype, retain its results and report the exact dependency; do not start
parallel versions of upstream fixes just to keep producing code. Starting the
Sol session does not authorize taking over the existing task. Do not send it
messages without direct user authorization to message that task.

## 2. Current code — the remaining seams are real

The following findings were checked in the source, not reproduced on the
user's devices. They are an investigation baseline, not a fleet verdict.

| Surface | Anchor | Consequence |
|---|---|---|
| Server master | [`master_playlist_with_shape`](../../crates/plurxd/src/http/hls/playlist_text.rs) emits one `EXT-X-STREAM-INF` | The JSON ladder does not give the playback engine multiple video variants. |
| Web offer and fallback | [`requestQualityChange` / `fallBackDirectedChange`](../../crates/plurxd/src/web/player/directed-change.js) | A failed offer/alignment goes to a reopen, including while the incumbent is healthy. |
| Web exposure | [`commitPreparedReplacement` / `exposePreparedReplacementAtFrame`](../../crates/plurxd/src/web/player/prepared-replacement.js) | Two independent video/audio clocks; `PREPARED_ALIGN_SLACK_MS=250`; presentation proof then DOM layer/audio ownership swap. |
| Web retirement and measurement | [`prepared-switch-measurement.js`](../../crates/plurxd/src/web/player/prepared-switch-measurement.js) | Delayed teardown already exists; do not propose it as missing. Dropped counters are insufficient for a held or black frame. |
| Apple commit | [`commitPreparedSuccessor`](../../clients/apple/Sources/PlayerController.swift) | Removes the prepared item from its staging player, then calls `replaceCurrentItem` on the visible player. Preparation is not preservation of the display pipeline. |
| Android commit | [`commitPreparedReplacement`](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt) and [`PlayerScreen`](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt) | Pauses the predecessor and repoints `PlayerView` before the successor can report its first frame on that surface. |
| Wire | [`playback_control.rs`](../../crates/plurxd/src/playback_control.rs) | Strict enum/field validation, retained capabilities, sequence/epoch fencing; adding a field is not universally safe. |
| Producer | [`vodencode.rs`](../../crates/plurxd/src/vodencode.rs), [`vodserve.rs`](../../crates/plurxd/src/vodserve.rs) | Reuse rational frame-grid, immutable artifact publication and existing admission. Do not add another unmanaged FFmpeg launcher. |

Existing wire vocabulary, copied from the source in abridged form:

```rust
enum QualitySelection {
    Auto { height: Option<i64>, candidate_id: Option<CandidateId> },
    Original,
    Manual { height: i64 },
}
// DynamicCapabilities uses deny_unknown_fields.
// ControlAction::Prepare is advertised as "prepare_replacement".
// delivery.preparation is exactly "staging" | "offered" | "none".
```

Do not overload `candidate_id`: it already belongs to negotiated playback
route identity. Do not add a fourth `delivery.preparation` string. A new
rendition transaction must have independently negotiated vocabulary.

## 3. Decisions — continuous media and one decision owner

1. **Use multivariant HLS for compatible changes.** Keep one media element
   or native player item. The server may own several rendition producers,
   but the viewer still owns one playback session and one timeline.
2. **Keep audio continuous.** A video-only quality change references the same
   audio playlist, init and sample sequence. Independently encoding AAC for
   each video rung risks different priming, padding and clocks.
3. **Preserve the existing adaptation policy first.** In the hls.js path,
   Plurx chooses the rung and hls.js schedules the media switch. Do not run
   independent native ABR and Plurx Auto controllers against each other.
   Native engines that cannot expose equivalent control get an explicit
   adapter contract and feasibility result, not a guessed equivalent.
4. **Protect healthy playback from failed optional changes.** Failure is
   `retained_current`, not an automatic reopen. Genuine supply/decode failure
   still follows the existing recovery policy and budgets.
5. **Do not advertise cold work as ready.** Variant availability and selected
   fragment readiness are different facts. A controlled client prepares the
   target range before selecting the variant. An autonomous engine needs a
   proven bounded producer policy; listing all rungs is not that policy.
6. **No unbounded ladder fan-out.** Per playback, at most two active video
   rendition producers and one shared audio producer when needed. Controlled
   clients normally need one video producer outside overlap. An autonomous
   two-rung master instead reserves delivery capacity for both rungs for its
   entire attachment (§5.4); it cannot borrow optional capacity and promise
   arbitrary future selection. Count audio, decoder, GPU and source-reader
   costs explicitly.
7. **Separate requested, scheduled and presented quality.** Neither a menu
   tap nor a fetched segment changes the delivered badge or active producer
   attribution prematurely.

```text
 selection / Auto decision
            │
            ▼
 one quality intent owner ──▶ prepare target film interval
            │                         │
            │ failure                 │ media ready + intent still current
            ▼                         ▼
 retain current playback       schedule compatible video segments
                                      │
                              first target frame presented
                                      │
                              settle + retire old demand

 incompatible presentation ──▶ prepared pipeline replacement
 unhealthy incumbent ────────▶ existing recovery owner and budget
```

## 4. Intent and failure contract — every request settles once

### 4.1 One transaction spans preparation through presentation

Add a shared test fixture for the following state vocabulary, with adapters
in JS, Swift and Kotlin. These are proposed states, not existing wire names:

```text
requested → preparing → ready → scheduled → appended → presented
     └────────────┴────────┴───────┤                │
                    retained_current / superseded │
                                                  └→ observation_unknown
 actual playback failure ────────────────────────────▶ recovery_owned
```

`scheduled` reserves a boundary but has not appended target media.
`appended` is an irreversible local media commitment: the player may display
those bytes even if the viewer changes their mind. Every append records the
rendition, transaction, exact film interval and buffer/attachment identity.
Keep this provenance until the interval is consumed or demonstrably removed
by an independent seek/teardown. Superseding the newest **intent** does not
cancel reality already committed to the playback buffer.

Identity is `(playback_id, generation, control_epoch, intent_revision,
transaction_id)`. Revision increments on each new viewer/Auto command;
transport retries reuse the transaction. Existing request sequencing remains
independent. Capture current source position at use time, never reuse the
tap position as a reopen/seek position.

Keep three facts independently:

- **Preference:** saved Auto / Original / Manual height; remains the viewer's
  choice even if a particular attempt cannot be fulfilled.
- **Pending target:** this transaction's requested rendition and state.
- **Presented:** actual media rendition at the current film time.

Only a transaction with no target bytes appended may settle
`retained_current` or cancel all of its dependencies. After append, a newer
choice stops further old-target requests, but the existing interval remains
scheduled. The new choice starts at the next uncommitted boundary. The UI
may therefore briefly report an earlier scheduled rendition while showing a
newer pending choice. Presentation updates are ordered by film-time interval
and attachment, not simply by the newest intent revision.

A three-rung change can leave bytes from three renditions buffered without
three live producers. Release producer demand for intervals already fully
materialized, retain their artifact/reader pins, and admit at most two live
video producers. If that bound cannot be met, defer the newest request or
retain the currently scheduled media; never drop its pins to admit a third
encoder. Bound queued provenance to the existing forward-buffer byte/time
limits and refuse new scheduling rather than evict unresolved entries.

On `retained_current`, clear pending selection and the wire change trigger
without silently changing the saved preference. Otherwise every reporter
tick restages the same failed choice. A later explicit Retry/new selection
gets a new revision. Auto clears its switching latch and resumes the existing
cooldown/evidence policy; it cannot retry the same failed target every tick.

### 4.2 Failure routing is decided from current evidence

| Event | Healthy incumbent | Unhealthy incumbent |
|---|---|---|
| No offer, admission refusal, target timeout | Cancel target; retain current; manual choice receives a small nonblocking explanation and Retry. | Hand control to existing recovery policy once; use its cause, rung floor and reopen budget. |
| Target corrupt, incompatible or cannot align | Retain current; mark this attempt failed; never invent a capability opt-out. | Same recovery owner; no second fallback owner. |
| Target frame fails after visual exposure | Roll back only if the predecessor is still viable and aligned; otherwise recovery owns it. | Recovery owns it. Do not send `presented` for a black successor. |
| New choice | Supersede unappended work only; preserve committed intervals and schedule the latest choice after the append frontier. | The recovery owner receives the latest preference. |
| Seek, stop, navigation, end of title | The independent transport action owns buffer removal/attachment teardown and can release discarded intervals after it completes. No abandoned quality fallback. | The newer command owns the result. |
| Pause / resume / rate / mute / background | Preserve current transport intent; invalidate stale timing decisions. A paused switch may buffer, but cannot claim a newly presented moving frame. | Preserve existing lifecycle and recovery behavior. |

Use existing playback-surface and supply/decode evidence; **quality-target
failure is never evidence that the incumbent failed**. Health changes while
waiting are handled at that moment. Unknown health does not authorize an
optional destructive reopen. Expose a deliberate "Apply with restart" action
for a manual request that cannot transition continuously; show its cost and
sample the then-current playhead. Do not silently repurpose Retry as restart.

The present 12,000 ms offer budget is retained for optional preparation.
Before append, a 30,000 ms active-time control budget bounds scheduling and
revalidation. Missing that budget safely retains the already scheduled
playback and cancels only the unappended target. Before appending, compute
and record the expected presentation time from boundary PTS, current PTS
and playback rate. Healthy prebuffer may make that time more than 30 seconds
from the tap; report the actual expected delay rather than promising a false
30-second presentation bound or flushing playable media.

After append, the observation deadline is expected boundary presentation
plus 2,000 ms of active wall time, recalculated on rate changes. Missing it
means `observation_unknown` or genuine recovery, never `retained_current`.
Continue observing committed provenance without restaging the request; a
late frame can still resolve its presentation receipt. Pause suspends this
observation clock. Producer leases remain finite: paused playback renews
bounded artifact retention through the parent lifecycle and does not keep
encoding indefinitely. End-of-title before any append settles
`retained_current/no_remaining_boundary`; if bytes are already appended,
end-of-title records whether their interval was consumed and closes it.

### 4.3 Cancellation must work before an offer exists

The old reopen incidentally superseded the predecessor and canceled its
speculative producer. Retaining the incumbent removes that cancellation edge.
Therefore CQ2a supplies an explicit cancel-by-intent operation, negotiated
with the owner, before CQ1 consumes it. It covers planning, reservation,
queued admission, priming, offered and scheduled-but-unappended work.
Cancellation is idempotent and cannot cancel a newer revision or release
committed media dependencies. For an appended transaction it cancels only
future unappended work and returns the committed intervals it retained.
An old owner must not resurrect work after an epoch change.

For old servers without this operation, restore the accepted control
selection to the incumbent and stop repeating the failed ask; use an action
abort when an offer exists. Do not claim one legacy deadline covers all
work: the durable 330-second deadline starts during staging, while detached
planning precedes it. Legacy cleanup retains its existing behavior. Test a
late planning completion after incumbent selection has been restored; record
any lingering work honestly. Immediate cancel-before-offer requires the
negotiated CQ2a owner implementation.

## 5. Media contract — one presentation, aligned renditions

### 5.1 Compatibility is more than matching file and height

The server resolves a presentation family from actual output facts:
source revision · video codec/sample-entry family · dynamic range and color
metadata · bit depth · rational frame grid · timeline/edit origin · audio
track/codec/layout/sample rate/delay · subtitle mode and burn identity ·
encryption identity, if any. Width, height and video bitrate vary inside the
family. Encoder-specific parameter sets may vary where the player supports
reconfiguration; they are not assumed byte-identical.

M3 starts with H.264 SDR video and a shared AAC audio track. Silent media is
a separate tested case. HEVC, HDR, VFR normalization and source-copy join are
separate compatibility rows; an unverified join is not silently classified
as compatible based on extensions or MIME type. Keep original source quality
and existing audio capabilities on presentations outside the initial family.

### 5.2 Exact encoded-media invariants

- Use the existing rational frame grid and segment planner. Never hardcode
  a remembered two- or four-second segment length.
- A rendition's segment N covers the same source-time interval as segment N
  in every compatible rendition. Validate decoded PTS as well as manifest
  durations; `start_number` alone proves nothing.
- Every advertised switching boundary starts with independently decodable
  video. Verify closed-GOP/random-access behavior for each encoder; inspect
  SPS/PPS or equivalent sample entries, DTS/PTS, B-frame offsets, sample
  durations and first/last frame identities across the join.
- Keep immutable, rendition-specific init URLs. Resolution-dependent init
  bytes must never overwrite an earlier init at the same URL.
- Preserve HLS media sequence, discontinuity sequence, film-time origin and
  VOD duration across renditions. A discontinuity is not an excuse to reset
  timestamps during an otherwise compatible quality change.
- The shared audio playlist publishes each sample once on its own exact
  sample clock. AAC priming, encoder delay, padding and final trimming must
  remain consistent; the video switch cannot restart the audio encoder.
- Preserve subtitle timing, A/V offset, chapters, playback rate and seek
  coordinates. Burn changes leave this family and use replacement.

Encode deterministic fixtures with numbered moving frames and a continuous
audio reference. Decode joins at every supported boundary, including a cold
start in the middle, non-zero source timestamps, 24000/1001 fps, B-frames,
the final short segment and a seek followed immediately by a quality choice.

### 5.3 Parent session, child renditions and route identity

Extend the existing session/producer ownership model rather than turning
every rung into another public playback session. One parent keeps the
viewer lease, watch progress, resume state, authorization and control epoch.
Children own immutable artifact/recipe identity, producer lease and demand.
Reuse the current Store and actor conventions; durable publication requires
epoch fencing, whereas the loaded client buffer is not durable state.

Proposed negotiated route shape (new routes, not claims about existing API):

```text
/api/v1/hls/{parent}/master.m3u8
/api/v1/hls/{parent}/video/{rendition}/index.m3u8
/api/v1/hls/{parent}/video/{rendition}/init/{artifact}.mp4
/api/v1/hls/{parent}/video/{rendition}/segment/{number}.m4s
/api/v1/hls/{parent}/audio/{audio_rendition}/index.m3u8
```

`rendition` is an opaque server-issued identifier bound to the family and
recipe, not a client-supplied arbitrary height/path. Child routes inherit
parent authorization; possessing a cache key grants no read access. Range
requests, relay, cancellation, cache hits and HEAD follow existing media
authorization. Never put a bearer, source path or full credential URL in
logs or retained evidence. Old single-rendition routes remain valid.

One control reporter renews parent playback; child media requests renew only
the demand they actually create. Browser prefetch/download is not proof of
presentation. Child cancellation never retires the parent or another
viewer's shared artifact. Retention pins cover the incumbent's playable
interval and every committed target interval until it is consumed or removed
by a completed transport action. Cancellation releases only uncommitted
ranges; presentation of the first target frame alone does not unpin the
rest of its buffered interval.

### 5.4 JIT preparation has bounded ownership and honest availability

For controlled switches, the client reports its **append frontier**, the end
of video already committed to the playback buffer, separately from playhead
and buffered runway. The server primes the target from the next valid
boundary at or beyond that frontier, through at least two complete segments.
The client rechecks coverage if loading advanced while priming. If the
boundary is now stale, move preparation forward within the original budget;
do not seek backward, erase healthy buffer or keep chasing without a bound.

The manifest enumerates a stable compatible family for controlled clients.
Only the client-selected, prepared rendition may drive a controlled load;
disable the competing hls.js ABR selector. Every enumerated URI still needs
a real bounded media-serving path for initial selection, retries and seeks.
No empty/fake playlist, permanent 404, or unbounded HTTP wait for cold media.

Autonomous engines may fetch any advertised variant and need a distinct
policy. M0 must demonstrate what native Safari/AVPlayer actually requests,
including initial selection, seeks, upgrade, downgrade and resource denial.
Dynamic master refresh cannot be assumed; do not depend on removing an
unready variant from a master the player already cached. Start autonomous
experiments with two real renditions and warm media around current demand.
An autonomous stable master must retain admission entitlement and renewable
demand coverage for **both** advertised rungs for the entire attachment,
including seeks and later reselection. Workers may idle behind sufficient
runway, but capacity cannot be released as speculative work five seconds
after the first switch. Account for both video entitlements and shared audio
before attaching this master. These are foreground delivery commitments;
new foreground demand cannot silently preempt the inactive advertised rung.
This can reduce concurrent viewers and must be measured and documented.

If this capacity cannot be admitted up front, choose the existing prepared
presentation before attachment and report the reason. If infrastructure
failure breaks an admitted entitlement later, route it as playback recovery,
not a claimed harmless optional refusal. CQ0 tests pressure **after** the
first switch, then reselection and a seek into a cold interval. A cached
master remains a delivery obligation. Runtime delivery inability is distinct
from advisory qualification status and never changes the saved setting.

Per-parent producer bounds:

| Resource | Limit / settlement |
|---|---|
| Controlled active video work | At most two producers; third choices cancel unappended work or wait. Materialized committed intervals can outlive their producer. |
| Autonomous video work | Admission for both advertised rungs lasts until presentation teardown; at most two producers, with normal demand pacing. |
| Shared audio work | At most one audio producer for the selected audio identity; count its CPU/source-reader admission separately. |
| Optional preparation | 12,000 ms from intent to media ready; cancel rather than extend on retries. |
| Controlled old video after switch | Release producer demand once all promised intervals are materialized and in-flight reads settle; at most 5,000 ms cleanup grace after that point. Preserve committed interval/shared-reader pins independently. |
| Autonomous old video after switch | Do not apply the five-second release rule: it remains advertised and its admission/demand coverage lasts for the attachment. |
| Lost client / lost acknowledgement | Parent/child lease expiry bounds work; no permanent warm ladder. |
| Foreground contention | Controlled unappended preparation is preemptible before incumbent/audio; appended obligations survive. Autonomous advertised rungs are reserved foreground commitments. No contention result changes the saved switch. |

Readiness checks media bytes and sustainable supply for the requested
interval, not just reservation of a GPU slot. In failure tests verify that
the second encode does not starve the first, even with a shared source reader
or a software decoder bottleneck. Do not pre-encode the whole library.

## 6. Wire and settlement — extending strict readers safely

### 6.1 Negotiate before sending new fields

After §1.2's dependency boundary, CQ2a reconciles the landed display-aware
Auto parser/advertisement/cancellation infrastructure and adds only missing
cancel-by-intent semantics. Reuse its negotiation entry point and deploy
floor rather than adding a parallel mechanism. CQ1 consumes that contract;
CQ2 later adds continuous-media transaction support. Cancellation support must be available
for ordinary prepared replacements outside compatible rendition families.

`ControlBootstrap`, `ControlRequestV1`, `ControlResponseV1` and
`DynamicCapabilities` are strict readers. Do not send an additive field to an
old one and assume it ignores it. The preferred discovery surface is the upstream negotiated owner/start
advertisement. If it cannot safely express independent cancellation support,
CQ2a proposes a versioned authenticated feature-discovery route in its delta
review; an old server's 404 means unsupported. Either mechanism must resolve
the active owner and return the intersection of ingress and owner support,
bound to playback/generation/control epoch. It advertises
`quality_cancel_v1` separately from `continuous_quality_v1`. Freeze the exact
reused/extended route and schema in CQ2a and document implemented changes in
the API reference.

Only a client with discovery support sends the negotiated opt-in header on
bootstrap/control requests, e.g. `Plurx-Control-Extensions: quality_cancel_v1`.
Only mutually opted-in responses include the new bootstrap/envelope fields;
old clients receive byte-compatible existing shapes. Relay must preserve the
opt-in and verify the owner's support. A control epoch/owner change
invalidates cached support before any further extended request; rediscover
against the new owner. Unsupported ingress or owner produces a safe legacy
response, never forwarding an unknown field into a strict old parser.

CQ2a tests old/new client × old/new ingress × old/new owner, direct and
relayed routes, feature-discovery failure, epoch change between discovery
and use, and replay. The extension is rejected as unsupported before legacy
parsing if support races away. Unsupported never damages the parent stream.

Proposed bootstrap and control payloads; M2 freezes their exact schema in
cross-language fixtures before implementing producers:

```json
{
  "continuous_quality": {
    "version": 1,
    "mode": "client_selected",
    "family_id": "opaque-family",
    "renditions": [{"id": "opaque-rung", "height": 720}]
  }
}
```

```json
{
  "quality_transition": {
    "version": 1,
    "transaction_id": "opaque-transaction",
    "intent_revision": 7,
    "operation": "prepare",
    "target_rendition_id": "opaque-rung",
    "append_frontier_ms": 142000
  }
}
```

`operation` is `prepare | cancel_unappended | scheduled | appended |
presented | recovery_owned`. Existing playback/generation/epoch/sequence
fields fence the envelope. `scheduled` reserves the selected boundary and
range before append; `appended` reports the actual committed film interval,
rendition and attachment identity after append completes; `presented` adds
first presented film PTS and observation time. Reserving pins before append
prevents an acknowledgement race from deleting newly buffered media.
Unknown version/operation is rejected for that operation without damaging
the parent. Size and integer bounds follow existing validation style.

| State | Authority and response on retry |
|---|---|
| `requested` | Client-local until the owner accepts `prepare`. |
| `preparing`, `ready` | Owner response with identity, reason and verified ready interval. Replayed requests do not allocate again. |
| `scheduled` | Client proposes, owner acknowledges range/pin reservation before append. No claim that pixels changed. |
| `appended` | Client media fact; owner acknowledges receipt and preserves committed dependencies. Lost acknowledgement retries the same fact. |
| `presented` | Client observation; owner stores an idempotent receipt. Replay returns `presented` and its original identity, never a new preparation. |
| `retained_current`, `superseded` | Owner-acknowledged terminal cancellation only when no target interval was appended. A superseded intent with committed intervals returns `appended` plus `intent_superseded=true`. |
| `observation_unknown` | Client-local measurement result after append; owner retains `appended` until presentation, transport disposal or session closure settles it. |
| `recovery_owned` | Recovery coordinator takes transport authority; owner acknowledges and releases only dependencies the recovery actually discards. |

This is a new negotiated object, not `delivery.preparation` and not an
unadvertised `ControlAction`. CQ2a uses a smaller independently negotiated
cancel envelope keyed by the accepted quality-intent revision; it need not
implement rendition media. The server must not independently stage a
`prepare_replacement` for a selection already owned by a rendition transaction.
Keep bounded durable receipts using the existing control replay horizon;
never evict an unresolved appended interval to make room. Backpressure new
scheduling if needed. CQ2 freezes count/byte/retention bounds with tests.

Required race fixture: append completes while cancellation is in flight and
the `appended` acknowledgement is lost. The client cancellation carries its
latest committed-interval facts even if the owner last acknowledged only
`scheduled`. The owner conservatively retains the reserved interval until
append absence or transport disposal is established; a cancel response may
not release it based solely on its older acknowledged state. Retry/owner
failover must preserve the same dependency and never allocate again.

### 6.2 Commit is presentation, not a manifest request

Server state distinguishes target demand, scheduled future demand, and
presented rendition. Keep producing enough incumbent media until the target
range is deliverable and the client has scheduled it. Do not wait forever for
presentation before permitting more target segments; that would deadlock
consumption. Do not release incumbent pins just because target downloads
started. Retried settlements are idempotent. An earlier appended interval
may truthfully present after a newer intent arrives: record that media fact
without changing the newer desired choice or starting old work again. Closing playback after scheduling still releases all owned
children even if no frame was presented.

`effective_selection` must remain truthful to its documented meaning; add
explicit target and client-observed presentation fields instead of making
one existing height alternately mean requested, encoded and displayed. A
native player with no precise presentation callback reports unknown until
its supported observation establishes the fact; bytes served alone cannot
claim the screen changed.

Owner failover fences new work with the new epoch while already authorized
immutable reads and playable client buffer remain useful. Rebuild missing
child demand from durable parent/recipe state and fresh requests. A replayed
old owner's completion must not publish a new rendition or release current
pins. Test both Store backends and the actual relay path.

## 7. Client implementation — web first, platform facts preserved

### 7.1 Existing web handoff

Replace the fixed 250 ms acceptance tolerance with a frame-duration-derived
criterion. Compare presented media PTS, mapped to film time, and expected
display time where available. Account for playback rate; nearest acceptable
successor frame is at most one source-frame interval away. Merely lowering
the constant can increase fallback failures, so keep the retain-current
policy and use a bounded future rendezvous if chasing seeks cannot align.

Keep both composited pictures at final geometry, prove advancing target
frames, then expose at the presentation boundary. Respect browsers that
throttle an occluded element. No second `play()` unless that engine requires
it; preserve mute/rate/volume and handle rejected unmute/play promises.
Retain the existing deferred predecessor retirement and revalidate rollback
against the latest viewer intent. A dual-element audio swap is measured as
a separate seam; do not introduce a permanent Web Audio graph merely to
obtain a green diagnostic.

### 7.2 hls.js continuous path

The inspected vendored version is **1.6.16**. Work against that version;
upgrading it is not a prerequisite or a hidden part of this project.
[Its quality API](https://github.com/video-dev/hls.js/blob/v1.6.16/docs/API.md#quality-switch-control-api)
distinguishes immediate buffer-flushing switches from selection of future
loads. Use the future-load mechanism after preparation; verify its actual
buffer and event behavior in the vendored implementation.

Keep the same `<video>`, Hls instance, MediaSource and audio SourceBuffer
through an eligible switch. Do not call `play()`, `loadSource`, `detachMedia`,
`currentLevel`'s immediate setter, `currentTime=…`, or destroy the instance
as the normal quality-change action. Adapt the existing Auto decision into
an explicit scheduled level; one decision owner governs manual and Auto.

M0 must establish a loader/scheduler seam that pauses **future incumbent
video requests** at the selected boundary without pausing audio or playback.
No global `stopLoad()` to manufacture that boundary. If hls.js cannot hold
that frontier through public APIs, implement and test the narrowest loader
adapter against 1.6.16; do not build a second demuxer or patch minified code.
Cancellation resumes incumbent requests before its playable runway is spent.

Observe actual active fragments together with frame callbacks. A level
selection or `FRAG_LOADED` alone is not presentation. Update subtitle, stats,
watch progress and recovery consumers so they keep the parent identity and
read the presented rendition separately. Preserve existing native Safari
selection rather than forcing it onto MSE without device evidence.

### 7.3 Native Safari and Apple

AVPlayer's bitrate/resolution preferences must not be treated as an exact
manual-rung contract. Inspect the supported OS APIs and prove behavior with
the M0 fixture. Do not relabel an exact manual choice as a ceiling without a
documented product decision. Native Safari exposes different control than
hls.js and needs its own adapter result.

For a supported continuous mode, retain the AVPlayer/AVPlayerItem and layer;
use the compatible multivariant presentation with one quality-policy owner.
For manual modes that cannot select an exact variant in place, keep the
prepared path and build a two-player/two-layer handoff: the target must
render on its own layer before taking the picture. Do not transfer its item
into the incumbent player. Integrate observers, audio session, remote
commands, PiP/AirPlay, subtitle overlays and view identity with player
ownership; make the old player recoverable until commit.

Use a frame-boundary cut for aligned pictures; a crossfade may hide black
but can show two different film instants. Any audio overlap must be aligned,
bounded and measured for both silence and double sound. Devices unable to
run both decoders retain current playback or offer explicit restart, rather
than becoming black during an optimistic attempt.

### 7.4 Android

For compatible media, keep the ExoPlayer, MediaItem, MediaSession and attached
surface. Select the prepared track through Media3 track-selection APIs;
verify decoder adaptive support on the actual device and ensure another
selector is not simultaneously undoing the choice.
[Media3 track selection](https://developer.android.com/media/media3/exoplayer/track-selection)
supports constraints and exact track overrides; track identity must be
re-resolved when groups change.

For replacement, keep a target surface attached while preparing if the
platform supports it. Do not park the visible predecessor before target
presentation evidence. Treat SurfaceView and TextureView compositor behavior
separately; changing every device to TextureView carries power/HDR costs and
is not an unmeasured shortcut. Transfer media-session and listener ownership
at the real commit, with pause/background/close races tested.

## 8. Settings, telemetry and acceptance — prove what the viewer sees

### 8.1 Settings lifecycle

Use an explicit `continuous_quality_switching` setting in Settings →
Developer while the feature is unfinished, default off for the new path.
Keep the existing prepared-handoff setting and saved values intact. Readiness
describes missing platform evidence, media-family support and resource cost;
it never disables a switch, rejects Save, or overrides a saved opt-in.
Runtime media incompatibility/admission refusal is an attempt result, not a
silent configuration change. Graduate the control when the work is complete,
following [the Developer lifecycle](../features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md#developer-lifecycle--every-card-graduates).

### 8.2 Every switch produces one redacted record

Capture transaction and build identity, platform/OS/device/browser, family,
requested/presented rendition, manual/Auto/recovery cause, mechanism,
preparation and scheduling times, boundary PTS, first presented target PTS,
retained/fallback reason, cancellation/cleanup outcome and measurement coverage.
Also capture parent/child producer counts, queue wait, encode speed, buffer
runway and source-reader pressure before/during/after the switch.

Use four clocks explicitly: source media PTS, player-local media time,
client monotonic observation time and server monotonic elapsed time. Never
subtract unrelated host clocks to report a latency. Persist correlation IDs,
not full credential URLs. Metrics use bounded reason/platform labels; IDs go
in redacted event records, not metric label cardinality.

### 8.3 Acceptance is a per-switch result, not an average

Proposed product targets, to be measured rather than asserted as current:

| Measurement | Acceptance for healthy compatible switches |
|---|---|
| Playback surface | Zero black/empty frames, zero preparing overlays. |
| Video continuity | No backward step or skipped source frames at the join; no switch-induced excess frame gap greater than one nominal display interval over the pre-switch cadence. |
| Audio continuity | No introduced silence or overlap longer than 20 ms; no click/pop or audible seam in the captured reference. |
| Player identity | Same element/item and attachment for continuous switches; zero reopen/seek/reset calls. |
| Presentation result | Requested exact manual rung is observed, or a truthful retained/unsupported result; never a false success. |
| Lifetime | One terminal outcome; no leaked producer, child lease, media observer or pending callback. |
| Latency | Report tap → ready → scheduled boundary → presented, including buffer delay; 12 s optional preparation and §4 presentation bound are enforced. |

A retained-current outcome is a safe failure, **not a successful continuous
switch**. The success series requires 20 consecutive completed transitions
per platform and tested family, alternating up/down and including at least
five Auto changes where Auto exists. Record safe refusals separately and
do not silently retry until twenty successes are collected. A failure ends
that series; fix and run a new fully identified series.

Synthetic media has burned-in frame numbers, moving nonblack patterns and
continuous known audio, allowing independent capture to detect freeze,
black, repeats, skips, silence and duplicate audio. Capture at sufficient
cadence and calibrate the capture itself with a no-switch baseline; record
capture drops. Browser frame callbacks and dropped-frame counters are useful
diagnostics, but cannot alone prove pixels reached the display. Access-log
stall counters cannot prove sample-level audio continuity. Use test-only
audio capture or external loopback/HDMI capture rather than rewiring normal
production audio solely for measurement. Missing capture is `not measured`.

Required matrix: Chrome and Firefox desktop; Safari macOS native HLS; Safari
iOS; iOS app; tvOS physical device; Android phone and TV. Test normal and
fullscreen, muted/unmuted, subtitles, 0.5×/1×/2× rate, long prebuffer, rapid
choices, pause/resume, seek collision, background/foreground and end-of-title.
Measure decoder-limited and network-cliff cases separately from healthy
quality changes. Add one real high-bitrate title and one low-end device to
avoid qualifying only the synthetic fixture on the fastest machine.

## 9. Milestones — reviewable tasks on one effort branch

File ownership below is an execution map, **not** a disjoint-file exception.
Tasks overlap and must branch from the latest effort, merge serially into
it, and reverify after integration. No task goes directly to main.

| Task | Owned surfaces | Deliverable and acceptance |
|---|---|---|
| CQ0 — baseline and feasibility | Playback lab, fixtures, this document/evidence; temporary isolated prototypes | Capture current web failures and distinguish prepared/fallback. Prove 1.6.16 same-element two-rung switch with shared audio, bounded append-frontier control, real Safari behavior and native selection semantics. Record encoded joins and denied-admission behavior. This is a runnable prototype, not a literature review. |
| CQ2a — reconcile cancellation negotiation | Landed upstream strict parsers/owner advertisement, relay and readers | After §1.2, reuse the existing negotiation stack; add only missing independent cancel-before-offer semantics. Prove the mixed-version/owner-change matrix. |
| CQ1 — verify upstream safety and fill manual gaps | Landed prepared coordinators and their existing fixtures | Consume upstream voluntary-upgrade retention/admission fixes. Implement only missing manual-change behavior and continuous-media integration; verify recovery owns failure once and cancellation cannot restage/leak. Do not duplicate upstream reducers or tests. |
| CQ2 — protocol and ownership | Playback control, session bootstrap/relay, media-session ownership, client parsers | Versioned opt-in, parent/child identity, transitions and mixed-version fixtures. Prove unsupported peers receive no new strict fields and old epochs cannot settle new work. Freeze schema before CQ4. |
| CQ3 — compatible media and shared audio | VOD encode/serve, recipe/cache identity, playlist construction, FFmpeg fixture tests | Two real H.264 SDR rungs, shared audio, immutable init maps and rational-grid joins. Verify decoded boundary media, seek/tail/VFR cases and no cache collisions. |
| CQ4 — bounded JIT rendition serving | HLS routes, VOD demand, admission, Store/relay, cancellation | Prime at append frontier; one incumbent plus one target, shared audio accounted; denial retains current. Test rapid selection, disconnect, late producer completion, node loss and source-reader contention. |
| CQ5 — web continuous switching | Web player/session/menus/Auto, served-asset registration if needed, browser harness | Same element/Hls/MediaSource/audio buffer; future-load selection; no competing ABR; request versus presented UI. Complete 20-switch Chrome/Firefox series and keep separate Safari result. |
| CQ6 — prepared presentation repair | Web prepared exposure, Swift player/layer ownership, Kotlin player/surface ownership | Frame-derived alignment and target presentation before exposure; safe rollback, audio measurement and lifecycle tests. No change claimed complete using only mocked callbacks. |
| CQ7 — native continuous adapters | Apple and Android quality/session/control adapters, native tests and settings | Adopt continuous mode only with actual runtime/API support; exact manual semantics preserved. Complete native Safari, iOS/tvOS and Android evidence; unresolved API limitations remain explicit incomplete rows. |
| CQ8 — qualification and graduation | Docs/reference updates, settings graduation, validation scope, final evidence | Freeze task merges, integrate current main, run exact-tree Main promotion gate and retain qualification receipt plus all required device evidence. Only then promote and claim complete. |

Execution order is CQ0 → CQ2a → CQ1 → CQ2 → CQ3 → CQ4 → CQ5 → CQ6 →
CQ7 → CQ8, with §1.2's upstream landing/reconciliation boundary between CQ0
and CQ2a. CQ0 may discover an API constraint that changes CQ2–CQ7. Resolve that by
amending this document and obtaining an adversarial delta review before
implementing the affected design. Continue independently owned CQ0 measurement work; CQ1 production edits
remain behind §1.2's dependency boundary. Do not turn an unsupported native mode into a claimed completed task.
There is no permission checkpoint for routine implementation decisions.

### 9.1 Concrete source entry points

Server: [`http/hls`](../../crates/plurxd/src/http/hls.rs) and its
[`preparation`](../../crates/plurxd/src/http/hls/preparation.rs),
[`control`](../../crates/plurxd/src/http/hls/control.rs),
[`playlist`](../../crates/plurxd/src/http/hls/playlist.rs),
[`playlist text`](../../crates/plurxd/src/http/hls/playlist_text.rs),
[`relay`](../../crates/plurxd/src/http/hls/relay.rs) modules;
[`media sessions`](../../crates/plurxd/src/media_sessions.rs),
[`transcode ladder`](../../crates/plurxd/src/transcode/ladder.rs),
[`VOD serving`](../../crates/plurxd/src/vodserve.rs),
[`VOD encoding`](../../crates/plurxd/src/vodencode.rs), and
[`core transcode`](../../crates/plurx-core/src/transcode/mod.rs).

Clients: use the anchors in §2 and
[`menus.js`](../../crates/plurxd/src/web/player/menus.js),
[`session.js`](../../crates/plurxd/src/web/player/session.js),
[`player.js`](../../crates/plurxd/src/web/player/player.js),
[`PreparedReplacement.swift`](../../clients/apple/Sources/PreparedReplacement.swift),
[`PlayerView.swift`](../../clients/apple/Sources/PlayerView.swift),
[`PreparedReplacement.kt`](../../clients/android/app/src/main/java/tv/plurx/app/player/PreparedReplacement.kt).
New web scripts require file, `WEB_ASSETS`, shell tag and layout-table row in
one commit, in plain shared scope; no imports/exports.

Test anchors: [`web-control.test.js`](../../tests/playback/web-control.test.js),
[`preparation-measurement.test.js`](../../tests/playback/preparation-measurement.test.js),
[`web-policy.test.js`](../../tests/playback/web-policy.test.js),
[`playback-lab`](../../scripts/playback-lab),
[`PreparedReplacementTests.swift`](../../clients/apple/Tests/PreparedReplacementTests.swift),
[`PreparedReplacementTest.kt`](../../clients/android/app/src/test/java/tv/plurx/app/player/PreparedReplacementTest.kt),
HLS module tests and VOD encode tests. Extend behavior seams; do not add
string-presence tests as proof of media continuity.

### 9.2 Compiler and merge workflow

Before editing Rust, verify `rustc --version` is **1.97.1** and establish a
working loop. If unavailable locally, follow the
[source-only compile loop](../ci/AGENT-COMPILE-LOOP.md): archive committed
source, transfer neither `.git` nor credentials, keep `target/` warm, and
verify the exact integrated branch again before pushing.

Existing commands, selected according to changed surfaces:

```bash
rustc --version                                  # Verify the repository pin.
cargo fmt --all --check                           # Check formatting.
cargo check --workspace --locked --all-targets    # Compile affected integration.
cargo clippy --workspace --locked --all-targets -- -D warnings
node --test tests/playback/web-control.test.js \
  tests/playback/preparation-measurement.test.js \
  tests/playback/web-policy.test.js                # Current focused web seams.
python3 -m unittest tests.operations.test_docs_index
make web-check                                   # Served syntax/contracts.
make unit-core                                   # Includes hiqlite-store.
make apple-build                                 # iOS and tvOS compilation.
make android                                     # Android debug compilation.
```

The existing playback lab already has a `quality-cycle` case in
[`cases.json`](../../tests/playback/cases.json), plus 100 ms p95 / 250 ms max
video-gap checks and an explicit missing-audio warning. After §1.2's boundary,
extend that harness; do not create a parallel success oracle. Immediate CQ0
writes only the new investigative files allowed by §1.2, and does not edit
that harness or its existing cases. Its current Original↔720p case
crosses delivery families and cannot stand in for compatible 720p↔1080p
switches. Retain that distinction in case names and evidence.

```bash
scripts/playback-lab doctor --browser chrome
scripts/playback-lab plan --suite full --case quality-cycle
# After upstream integration, port CQ0 probes into compatible-rung cases.
```

Add exact focused regression commands per task; broad compilation is not a
regression. Core storage tests must use `make unit-core` or
`--features hiqlite-store`. Run the relevant native unit tests and physical
tests in addition to compilation. The new browser scenario and fixture
commands are deliverables of CQ0 and must be written into §10 once real;
do not invent passing invocations now.

Install the tracked hook with `make hooks` in a normal checkout. In a linked
worktree, `.git` is a file: install the same `scripts/pre-commit` at the path
returned by `git rev-parse --git-path hooks/pre-commit` if not already present;
do not bypass it or change its checks. Commit normally, and use `fix(`
or `perf(` for every user-observable behavior change. Every task PR names
one actual test per `Regression-Test: <path>::<test name>` line and records
its local command. Preserve those lines in the landing commit, including
`MergeMessageField` for Forgejo API merges. Attach every created PR to the
building session. `Effort development gate` blocks task integration;
`Main promotion gate` plus the exact-candidate receipt blocks promotion.
Requalify if main or the effort moves. Do not use CI as a compiler.

## 10. Evidence ledger — empty rows are unfinished work

The implementation session maintains this ledger and stores redacted machine
receipts in the existing evidence area. Every new prose document needs an
index row in the same commit. Update the earlier continuity plan/results and
adaptive-quality Phase 3 to point here as their successor, without rewriting
historical measured outcomes as current evidence.

| Milestone | Commit/tree | Focused command / device run | Result |
|---|---|---|---|
| Upstream ownership | PR #669, main `91917940e`; integrated tree `7c2a950ef` | Forgejo merged receipt and upstream completed status, 2026-10-01; pinned `cargo check --workspace --locked --all-targets` | Dependency released; integrated source compiles |
| CQ0 | `codex/continuous-quality-cq0`, planning base `ea5f76d34`; source hashes retained per run | New isolated lab; commands and limitations below | Runnable Chrome mechanics probe; native Safari and output captures incomplete |
| CQ2a | Integrated upstream route-v1 floor; independent cancellation envelope being built | Pinned workspace/all-target compile; negotiation and exact-cancel regressions authored, not run | Separate bounded public/peer route, exact accepted intent fences and durable cleanup receipts implemented; rapid/pause/seek, disposal, mixed-owner routing and old-server explicit fallback qualified; broader matrix remains |
| CQ1 | Integrated upstream retention/admission | Pending-planning cancellation regression authored; not run | Upstream safety reused; manual retention, normal-pool target-load Retry and older-server Apply with restart qualified; broader matrix remains |
| CQ2 | Strict transaction ledger, owner-fenced storage and serving integration implemented | Pinned workspace/all-target compile; lost append, replay, takeover, pin-pressure and cross-language fixture regressions authored, unrun | Dependency reservations, physical pins and web adapter implemented; pressure/takeover qualification remains |
| CQ3 | Verified two-rung AVC/shared-AAC family implemented | Actual isolated Linux init verification and production probes | BT.709 proof passes; continuity qualification remains |
| CQ4 | Controlled cold admission and demand retirement implemented | Pinned all-target compilation; regressions authored | Measured cleanup and pressure qualification remain |
| CQ5 | Production hls.js enrollment, reserved loader and observers implemented | Exact-source Chrome probes; first frame and first rung observed | Full Chrome fifteen-manual/five-Auto campaign passes on recorded source; Firefox full campaign remains failed and independent optical qualification is ongoing |
| CQ6 | Warm prepared surfaces and original overlap clocks implemented | iOS/tvOS and Android source compilation | Unit execution deferred; physical qualification remains |
| CQ7 | Public API and SDK constraints audited; prepared path retained | Official variant/track API documentation; device inventory | Android continuous adapter implemented and sources compile; Apple policy ownership and physical device evidence remain unfinished |
| CQ8 | — | — | Not run |

### 10.1 CQ0 isolated experiment — mechanics evidence, capture incomplete

The new [probe](../../scripts/continuous-quality-lab.mjs) and its
[receipt checks](../../tests/playback/continuous-quality/receipt.test.mjs)
write only this independent clone's `target/continuous-quality/`. They
serve the vendored hls.js **1.6.16** read-only, with one HTML media element,
one Hls attachment, two H.264 SDR fMP4 video rungs and one separately encoded
shared AAC audio playlist. The generated fixture has moving patterns and a
numbered frame lane, 24000/1001 fps, closed 48-frame GOPs, B-frames and a
short final segment. These fixed synthetic settings are investigative;
production must reuse the repository's actual segment planner.

```bash
node scripts/continuous-quality-lab.mjs fixture
node scripts/continuous-quality-lab.mjs verify-media
node scripts/continuous-quality-lab.mjs verify-joins
node scripts/continuous-quality-lab.mjs run chrome \
  baseline switch cancel-before-append cancel-after-append denied long-buffer
# Run receipt/unit checks at the user-directed final test stage:
CQ_RECEIPTS=target/continuous-quality \
  node --test tests/playback/continuous-quality/receipt.test.mjs
node scripts/continuous-quality-lab.mjs run chrome audio-baseline audio-switch
node scripts/continuous-quality-lab.mjs run safari native-autonomous
# Manual local reproduction, including a native Safari attempt:
node scripts/continuous-quality-lab.mjs serve
# Open the printed localhost URL with ?native=1, then click Start fixture.
```

The final numbered fixture SHA-256 is
`44ac3add25f0322c7f5f5f5f5a207d862c369a323e01159562f35c1baec86231`;
FFmpeg is 9.0.1 on this host. Each browser receipt pins its user agent,
fixture hash, Git planning anchor and probe source hash. The planning anchor
excludes the then-uncommitted prototype; `source-evidence.json` separately
pins the runner, page, probe, checks and read-only hls.js by content hash
and identifies the staged candidate tree. The normal commit supplies source identity after the catalog registration. Raw JSON, request
logs, append range changes, canvas diagnostics and browser screenshots
remain under the private generated target directory; no fleet credentials
or media source paths are needed. These are **not** promotion receipts.

**Observed constraints:**

- In 1.6.16, `nextLoadLevel` alone does not override a locked manual level.
  The initial experiment requested 1080p but kept loading 720p. Those trials
  are retained in `target/continuous-quality/initial-nextLoadLevel/`, but
  lack a complete source identity and are historical diagnostics only. The
  final runnable cases exercise `loadLevel`; the setter distinction is also
  visible in the read-only vendored code. No reproducible failed-setter
  regression or acceptance result is claimed from those initial trials.
  `loadLevel` sets the manual choice for future loads, keeps
  `autoLevelEnabled=false`, and does not invoke the immediate-switch buffer
  flush. Production adoption remains behind the integration delta review.
- Append provenance must use the actual video SourceBuffer, independently
  of the media element's audio/video intersection and playlist timestamps.
  B-frame rebasing makes the first buffered video time 0.083416 s in this
  fixture. Playlist fragment start 10.010000 s corresponds to actual
  SourceBuffer start 10.093416 s. The probe records before/after ranges from
  real append completion before hls.js completion listeners settle. The
  interval start is a **contiguous range-growth inference** from the old
  buffered end, not a decoded first-sample bound: overlapping replacement
  bytes could extend the same range. Only the first target segment is
  recorded as target provenance. General sample-level overwrite exclusion
  and per-interval pinning remain unproved, pending production integration.
- Cancellation before append is exercised while loading is stopped. After
  target append, the probe requests 720p again without removing media;
  already committed 1080p frames still present before the later 720p choice.
  Its result stays `observation_pending`, never false `retained_current`.
  This is local engine evidence; it does not implement or prove the CQ2a
  negotiated cancellation/acknowledgement-loss ownership protocol.
- With 30 seconds of forward buffering, a request at about 3.00 s first
  presents at 34.117416 s: 31.107 seconds after the tap in the final series. A post-append
  deadline based on tap time would falsely report retention. Existing media
  remains buffered and no quality-driven removal is observed.
- The local denied-selection case requests no target media and leaves the
  incumbent moving. It exercises client refusal mechanics only; resource
  admission, producer leases, JIT readiness and denied production work are
  **not measured**. The fixture is fully materialized before playback.
- All 48 independently decoded video segments align across the two rungs,
  including the short tail. Every decoded frame follows the rational grid;
  encoded shared AAC packet timestamps have zero gaps. This proves fixture
  timing, not sample-level audio output at the browser or device. The
  verifier decodes each segment separately; alternating-resolution encoded
  joins, burned frame-number recognition, DTS/sample-entry compatibility
  and cold midpoint joins are not qualified by these checks.
- The no-switch baseline and switch cases both report an initial
  `bufferSeekOverHole` to 0.133416 s; the fixture's startup A/V origin needs
  separate treatment before production-media qualification. It is retained
  as an observed startup limitation, not hidden as a switch success.

The six final Chrome attempts contain one no-switch baseline, two ordinary
safe refusals, two ordinary presented switches (one then returns to 720p),
and one long-buffer presented switch. Nine focused receipt checks pass with
no skipped cases when `CQ_RECEIPTS` is set. Stable callback-gap p95 is
50.1 ms; switch-window maximum is 50.1 ms for the regular/long-buffer cases
and 66.7 ms for post-append supersession. These are callback diagnostics,
not display-gap qualification. The prototype never starts live producers;
no claim is made about maximum live encoders or server cleanup.

**Follow-up actual append-sample experiment:** the new test-only
[MP4 inspector](../../tests/playback/continuous-quality/mp4-provenance.js)
reads the init track clock and shape, `tfhd`, `tfdt` and `trun` sample
metadata from the bytes actually passed to SourceBuffer. It includes signed
composition offsets and the current SourceBuffer timestamp offset. Every
completed media append records sample PTS coverage, shape, sample count,
intent and attachment identity; unsupported metadata is reported explicitly
rather than replaced by a successful range-growth inference.

New Chrome `switch`, `cancel-after-append` and `long-buffer` experiments
have zero sample-inspection errors. The first target's actual sample interval
is `[10.093416667, 12.095416667)` s for the ordinary/cancel cases, matching
the measured prior video frontier at 10.093416 s. Long-buffer actual target
samples begin at 34.117416667 s. Those new rows improve synthetic fMP4
append evidence, not device output or negotiated ownership. Historical
six-case receipts remain under `range-growth-series/`, matched to
`c16e515e1`; new receipts pin all runner/page/probe/inspector content hashes.
The old range-growth limitation remains part of the historical review.
The new parser supports only this inspected fMP4 shape: general trex-only
sample duration defaults, edit-list rebasing, encryption and other media
families remain outside its evidence. Added receipt assertions are written
but have not been run after the user's final-stage-only test directive.

**Follow-up encoded-join experiment:** `verify-joins` alternates all 24
segments between 720p and 1080p, preserving each rung's immutable init.
FFmpeg 9.0.1's HLS demuxer fails strict decoding when the playlist changes
fMP4 init maps; decoder errors are retained in `join-verification.json`.
This is a demuxer-specific failed experiment, not evidence that hls.js or
native Safari behaves the same way. After extraction with
`h264_mp4toannexb`, one elementary H.264 decoding pipeline accepts all 1,151
frames across 23 boundaries. Decoded source heights match every expected
rung and expose 23 resolution transitions. The elementary conversion
includes SPS/PPS but does not preserve MP4 sample timestamps, so this result
qualifies neither fMP4 timestamp joins nor audible/display continuity.
Burned frame recognition remains open. The earlier independent-segment and
Chrome results retain their `c16e515e1` source identity; the follow-up runner
has a separate `join-source-evidence.json` content/commit receipt.

**Decoded browser audio experiment:** `audio-baseline` and `audio-switch`
use a [test-only AudioWorklet](../../tests/playback/continuous-quality/audio-capture.js)
to retain decoded mono 48 kHz float PCM while deliberately outputting zero
samples to the destination. This changes the isolated fixture graph only;
normal production audio is untouched. Both captures have a contiguous
worklet clock and a maximum near-zero run of one sample after excluding
startup. Maximum adjacent-sample delta is 0.016371137 in both cases, with
RMS 0.0883437. The switched capture includes 819,071 steady samples and the
matched baseline 811,904. Captured `.f32le` files and SHA-256 identities
remain in the generated target directory. This is actual decoded PCM
capture, not speaker/HDMI loopback or audible-output qualification. It does
not detect every possible phase error and is limited to the synthetic
997 Hz reference. The added receipt assertion is deferred to final testing.

**Native Safari experiments:** `/usr/bin/safaridriver` starts, but
`POST /session` still times out after 15 seconds, including after unlock.
No automation preference or permission was changed. Direct computer-use
initially failed because the Mac was locked; Paul then unlocked it.
A new temporary Safari tab played the native master via a real Start click.
The bounded local POST collector retained `safari-ui.json` at 20.018 s,
with 476 video-frame callbacks, all 1920×1080, and no media error. Actual
requests included both video playlists/init maps and one shared audio
playlist. Native runtime reports one selected video track (`id=11`), with
empty kind/label and no exposed per-rung selection information.

Every observed frame in this run is 1080p. Fetching 720p does not prove
720p was presented, so **a native presented quality switch is not measured**.
An on-screen screenshot confirms a picture at the short tail, as a point
sample only. Exact manual rendition control, pressured reselection,
autonomous capacity reservation, native audible output and physical-device
qualification remain open. The temporary tab and fixture server were closed;
existing Safari tabs were preserved and the original selected tab restored.
The UI collector is a bounded local investigative receipt sink, not a new
production acceptance harness. Playwright WebKit fills none of these rows.

**How to read the results:** moving canvas pixels, callback PTS and rendition
size diagnose decoder progress; browser screenshots are point samples, not
external display capture. Physical browser output audio and continuous display capture are
**not measured**; decoded test-graph PCM is now captured separately. These muted synthetic runs do not qualify unmuted audio,
fullscreen, subtitles, pause/seek collisions, other rates, backgrounding,
real high-bitrate titles or low-end devices. Nonzero source origin, VFR,
HDR/HEVC, device selection and production JIT/admission remain unqualified.
CQ0 is runnable and retainable, with those evidence rows incomplete; it is
not a second acceptance oracle. Port the measurements into the existing
playback lab after §1.2 is satisfied.

**Ownership reconciliation, 2026-10-01:** the named upstream task is
complete and PR #669 landed on main as `91917940e` at 06:20:23 UTC.
The independent clone integrated that authoritative main in `7c2a950ef`.
Rust 1.97.1 `cargo check --workspace --locked --all-targets` passed against
that integrated source. Shared production work may now proceed. Reuse the
landed display-aware route-v1 worker/owner negotiation and voluntary
retain-current/admission policy. It does not yet provide an independent
cancel-by-intent extension or continuous rendition protocol. The existing
incumbent-wait cleanup takes registered successors but misses planning
candidates; CQ1 closes that gap through the same pending-candidate owner.
The upstream start advertisement cannot negotiate independent cancellation without changing strict reader shapes. The cancellation-only extension therefore uses a separate versioned, capability-authenticated quality-control route (API §10), with an exact-auth owner RPC and old-owner 404 fallback. Existing owner resolution is reused; there is no competing Auto reducer. Cancellation distinguishes initiation from replayable completed cleanup; durable markers fence late admission and commit on both storage backends. The web caller caches support by session/generation/epoch, holds the previous recipe on the wire during negotiation while keeping transport reports live, and verifies exact client/lifetime/recipe identity before cancelling. Its strict response, owner-change, old-ingress, coalesced-discovery and stale-intent regressions are authored but unrun. Completed cleanup receipts are bounded and exact, with immutable preparation bindings and inactive-parent retention cleanup. Cross-backend regressions cover late admission, commit fencing, changed replay/rejoin identity, premature cleanup acknowledgement and incumbent preservation; they remain unrun. The web client now replays the exact cancellation identity through a bounded acknowledgement-loss recovery and rejects unrelated receipts. Manual target failures retain the incumbent and standing wire selection while leaving the saved preference intact. Retry creates a new media revision; Apply with restart is explicit. Optional manual preparation is bounded and leaves incumbent loading active. These owner regressions are authored and remain unrun; continuous-media and native integration remain unfinished. Final adversarial
review follows the workflow below; no intermediate review or unit run.

**Workflow supersession, 2026-09-30:** Paul directed normal commits in the
independent clone, batched into one larger PR, with adversarial review only
when ready for main, followed by the fast lane. This also supersedes the older full
effort-promotion test timing for this session; missing device/media evidence
stays explicit. Per-task PRs, intermediate
focused-unit runs and the pre-integration adversarial review are superseded
for this session. Existing experiments and review remain historical evidence;
no new unit suites run during building. Compiler feedback and the normal
tracked commit hook remain in use. The non-overlap instruction and upstream
landing/release requirement remain intact. Routine decisions proceed with a
note rather than a new permission checkpoint; the new lab's single catalog
path is registered under `playback.pipeline` to satisfy the normal hook.
The [status page](CONTINUOUS-QUALITY-STATUS.html) carries current progress.

Each platform series records total attempts, presented switches, safe
refusals, recovery-owned outcomes, visible failures, capture coverage,
worst frame gap, audio discontinuity, maximum live producers and final
cleanup. Pin server SHA/runtime identity, client build, device/OS, fixture
hash, adapter mechanism, network profile and experiment settings.

**How to read it:** twenty successful state transitions with no captured
audio is video evidence only. Zero dropped frames with a held picture is a
failure. A quality badge that changes while the old rendition still plays
is a telemetry/UI failure. A quiet screen during denied admission is a safe
refusal, not proof that the quality changed. Unit tests protect ownership;
captured media and physical devices establish continuity.

## 11. Builder handoff — begin with evidence, then deliver the effort

The requested implementation model is **GPT-6.1 Sol**. Use the independent clone
`/Users/pjunod/code/plurx-agent/continuous-quality`, which contains this
reviewed plan and has its own Git metadata. Do not write to the user's
`/Users/pjunod/code/plurx` checkout, its shared target directory, or the other
session's clone. Create `effort/continuous-quality` with this planning commit;
refresh authoritative main and integrate upstream at §1.2's boundary. Do not
build on an unrelated task branch or modify its untracked files. Read the repository instructions and
the review dispositions before editing. Establish the compiler loop, then
start CQ0 within its narrow file allowance. Check the upstream dependency
before CQ2a/CQ1 and continue through the milestones only when the documented
ownership boundary is satisfied; routine permission for authorized work is
already granted.

This document authorizes the build and its ordinary task PR workflow. It
does not waive repository gates or turn absent physical-device access into
passing evidence. If a hardware/API constraint blocks a platform, record the
precise failed experiment and continue independent work. The checkout host was inspected during planning: Homebrew's default
`rustc` reports 1.98.0, but `rustup run 1.97.1 rustc --version` successfully
reports the required 1.97.1. Use the explicit pinned invocation (and verify
again in the builder), rather than concluding this host has no compiler.
The shared source-only cloud loop remains the fallback if local prerequisites
fail. A required design change receives an adversarial delta review. Do not deploy an unfinished
effort to the user's fleet to discover whether it compiles or plays.


### 10.3 Continuous-media transaction storage — October 1, 2026

The shared core owns a strict version-1 request, operation and receipt schema,
with its initial cross-language fixture in
`tests/playback/continuous-quality/contract.json`. This schema is separate
from legacy control readers. The operation object is tagged by `kind` and
unknown fields and operations fail strict decoding. No production discovery
advertises continuous support yet: serving and client adapters remain work.

One generation and attachment initially retained 16 transactions and 64
reserved intervals (128 since §10.75),
256 MiB of reserved media, and 128 replay receipts. The complete durable JSON
is capped at 128 KiB. Exact receipt replay lasts 90 seconds; expiry removes
replay copies, never unresolved reserved intervals. Refusal applies atomically
and backpressures new scheduling rather than discarding committed facts.
SQLite migration 89 and replicated migration 67 add the ledger. Writes CAS
its revision under the active parent owner/epoch/lease. Takeover preserves
reserved and appended media facts while clearing old command authority and
requiring producer readiness to be verified again. Inactive-parent maintenance
cleans records in batches of 256 after the replay horizon; restore clears old
operational ledgers.

A cancellation carrying completed append facts first records those facts.
A cancellation with no append observation conservatively retains scheduled
dependencies. Only completed transport disposal releases its named intervals;
producer cleanup alone cannot prove a scheduled append absent. First
presentation and observation time remain separate from scheduling and append.
After disposal, bounded per-interval metadata may be removed, while the
`ever_appended` fact prevents a later cancellation from claiming retention.
The ledger records dependency reservations; physical cache-pin renewal and
producer serving are the next integration work, not evidence already supplied
by this storage model. All newly authored behavioral regressions remain unrun.

### 10.4 Shared soundtrack command construction

The encoder argument builder now honors a resolved video-only output: it
uses `-an`, omits the audio map and omits AAC encoder options. Continuous
rung planning must set `input_has_audio = false` before resolution so the
validated plan digest describes the actual artifact. Legacy argument
construction retains its existing optional audio mapping.

`vod_shared_audio_args` constructs the independent AAC-only fMP4 producer
from the selected audio recipe. It shares the existing film-global sample
lattice, seek preroll and offset correction with muxed VOD, uses an exact
selected audio map, and refuses silent sources or invalid execution bounds.
Video encoders cannot restart this producer during a rung change. The
existing publisher's AAC priming removal and timestamp restoration must
be integrated before these bytes can be served as the family soundtrack;
command construction alone is not publication or join qualification.

The encoder regression is authored in the existing VOD recipe test and
compiled without execution. Playback actor integration and decoded media
qualification remain outstanding.

### 10.5 Audio-only publication path

`vod_shared_audio_plan` supplies the soundtrack's own 48 kHz clock: each
ordinary interval has 94 AAC frames, and the last interval carries the
remaining declared samples. This grid is independent of every video rung.
The new `PlannedAudioSegmenter` accepts exactly one audio track, checks
contiguous absolute sample times and payload bounds before publication,
refuses gaps or overlaps, and keeps incomplete intervals unpublished.
Publication memory has a 16 MiB interval allowance and a 4 MiB incoming
fragment allowance; refusal does not evict an already published interval.

The existing VOD generation runner now recognizes an encoded audio-only
plan, applies its existing AAC priming removal, restores the absolute film
clock and passes samples to that cutter. Completed intervals use the usual
sink. A trailer that ends short of the promised final interval fails typed;
a killed producer leaves the pending interval unmaterialized.

Sample-preservation, identical restart publication and gap/overlap refusal
regressions are authored and compile. They have not been executed. Family
creation, audio admission/resource accounting, durable media pins and
playlist routing still need integration before this becomes a served
continuous presentation.

### 10.6 Durable dependency lookup

Both stores expose a bounded projection of exact reserved media intervals
from the atomic continuous ledger. Receipt pruning, producer retirement and
owner takeover do not remove those dependencies. An exact disposal CAS does.
The lookup rejects malformed projections or more than 4,096 intervals rather
than returning an incomplete retention set. Store contract regressions cover
takeover retention, an unrelated rendition and exact disposal release. They
are authored and compiled, not executed.

The GC bridge must serialize dependency installation against physical
eviction on the owning rendition. A lookup followed by an unguarded unlink
would race a newly scheduled interval, so the store projection alone is not
a completed physical pin implementation. Directory cleanup and shared cache
GC need the same rule before continuous delivery is advertised.

### 10.7 VOD retention bridge

Working-set eviction now obtains the rendition's existing exact-key build
gate, reads durable dependencies outside the manifest lock, then maps only
whole entries on the exact stored clock into protected windows. Invalid or
unavailable dependency facts retain bytes and retire a capacity-held producer
rather than overriding the retention obligation. Cached GETs can continue
while the store lookup runs. The gate is released before producer retirement.

Dormant purge, obsolete encoded-generation cleanup and adopted-init drift
now retain reserved directories and identities. An encoder restart or
missing marker cannot overwrite a reserved immutable URI; it refuses with
attachment repair needed. A matching head regeneration may still restore
the same init bytes. These retention reads do not depend on a live producer.

The schedule publisher must obtain those same sorted rendition gates while
verifying actual cached artifacts and committing a reservation. That writer
funnel and its HTTP/peer integration remain outstanding. Shared completed
cache publication also needs its existing consumer-pin integration; the VOD
retention bridge alone does not complete CQ2/CQ4. Exact-window eviction and
malformed-clock regressions are authored and compiled without execution.

### 10.8 Verified video family shape

Continuous video uses an explicit video-only H.264 High level 5.0 recipe,
with normalized square-pixel geometry and SDR output. Resolution and bitrate
remain rung facts; source object version, source facts, audio recipe, color,
codec and exact rational frame grid form the shared family identity. An
immutable rung records the hash of its actual init bytes. Families contain
two to eight distinct rung IDs and raster shapes.

Membership inspects the actual single AVC sample entry: codec triplet,
dimensions and BT.709 limited-range `nclx` facts must match the resolved
recipe, and the init must contain only its one video track on the exact
frame-grid clock. A declared family does not prove decoder join continuity.
Actual SPS constraints and platform joins still need qualification.

Admission checks level 5.0 macroblock size and rate limits using rational
integer arithmetic, and bounds the VBR peak. The limits follow the primary
[FFmpeg H.264 level table](https://www.ffmpeg.org/doxygen/4.4/h264__levels_8c_source.html).
Unsupported cadence, deinterlacing or output shape refuses this family recipe
and leaves prepared replacement available. Default recipes retain their
existing cache identities.

Actual-init inspection, family identity separation and exact level-boundary
regressions are authored. Workspace/all-target compilation is the current
verification; tests remain deferred. Production actor creation, resource
admission, audio pairing and delivery are still outstanding.

### 10.9 Frozen soundtrack recipe and worker bounds

`VodSharedAudioRecipe` freezes selected audio, correction, channels and rate
against the held source facts. Its digest excludes video resolution, video
bitrate and video encoder choice; changing a video rung leaves the soundtrack
identity intact. The existing shared-audio command entry point now derives
this recipe, and the recipe owns both its AAC sample-clock plan and argv.

The command caps its decoder, filter worker and AAC encoder to one thread
each. `VOD_SHARED_AUDIO_CPU_THREADS` names the three-thread pipeline admission
reservation. Production actor construction still must use that reservation;
no hardware video slot belongs to this audio producer. The authored recipe
regression checks identity reuse across changed video bitrate and each argv
thread cap. Compilation is verification at this stage; tests remain deferred.

### 10.10 Soundtrack role in immutable VOD

The frozen VOD encoding wrapper now carries an optional shared soundtrack
recipe. That role owns its audio-only sample plan, argv and separate artifact
namespace. The normal stored-plan builder and restarted generation use the
same role-aware plan. Admission and handoff calculations reserve three CPU
threads and no video hardware slot for this producer, regardless of the
parent video estimate. Video-only plan byte estimates exclude audio.

Audio work does not publish or invalidate video-candidate throughput proofs
and cannot satisfy a video-candidate cache offer. Existing video recipes
retain their artifact namespace and defaults. The authored integration
regression checks refusal at two CPU threads, admission at three with zero
hardware slots, cleanup of the permit and actual one-track audio init output.
It is compiled without execution.

The family actor still needs to construct this role, install shared audio
and video attachments and route their playlists. This wrapper is integrated
with generation and admission; it is not yet reachable as a continuous
client presentation.

### 10.11 Bounded continuous transport facts

The continuous transition request now validates its exact wire identity and
operation bounds independently of ledger state. Interval and artifact lists
are bounded and reject duplicate artifact IDs; SHA-256 identifiers use one
canonical lowercase representation. The reducer applies this validation
before changing any fact.

Receipts validate against the exact request sequence, owner, generation,
attachment and transaction. A prepare receipt must bind its requested intent
and target. Media facts must fit the ledger bounds, and a receipt cannot
claim retained-current or superseded after an append. Stored replay receipts
use the same check when decoding the ledger. Duplicate/oversized requests,
wrong sequences and attachments and false retained-current receipts have an
authored regression. HTTP/peer delivery and physical reservation verification
still need integration; this validator alone does not expose the feature.

### 10.12 Reserved bytes at publication

Producer publication now holds the same exact-rendition gate used by
retention and the forthcoming reservation writer. It reads dependencies
outside the manifest lock with a one-second lookup budget. A reserved entry
can be regenerated only with its exact whole-entry clock, byte length and
SHA-256 payload digest. Different bytes cannot replace its immutable URI.
Identical regeneration remains available, and unreserved neighboring entries
remain independent. Artifact IDs name the media payload; verified init
identity is a separate family fact.

The gate remains held through the existing atomic file/manifest publication
and is released before producer progress notifications. Source continuity
is checked again after dependency lookup. Unknown retention facts refuse
publication rather than overwriting a dependency; cached GETs do not wait on
that store lookup. This adds one bounded dependency query per publication,
which must be included in the final sustained-production qualification.

Exact regeneration, changed equal-length payload, wrong clock, partial
interval, neighboring publication and disposal release are covered by an
authored regression. The physical schedule writer and shared-cache pins
remain outstanding; this publication fence is not their completion.

### 10.13 Physical reservation publisher

`QualityReservationPublisher` defines the owner-side write contract; VOD
implements it with its existing sorted exact-rendition gates. Inputs are a
server-reduced candidate ledger, the observed store revision and a verified
family. They are not accepted as client-supplied ledger or family objects.
Both removed and retained dependency keys remain locked through the CAS.
Old dependencies survive takeover without requiring their producer to live.

A new reservation must name a locally attached, materialized video-only
continuous recipe. The held source, frozen object version and frame grid
must still match. The writer bounds init and media reads, verifies their
actual SHA-256 identities, parses the actual init and fragments, and refuses
extra tracks, changed init facts, non-clean starts, composition offsets,
off-grid samples, gaps, overlaps and incomplete intervals. It publishes only
through the store's exact parent-owner/epoch/revision fence.

The inherited caller deadline covers preflight and acknowledgement. Once a
store mutation is submitted, a settlement task retains the physical gates
until that operation finishes even if the caller leaves or times out. A lost
acknowledgement is therefore unknown, not an absence or cleanup receipt.
The store write uses current submission time after preflight for its lease
comparison. No reservation is acknowledged before the CAS completes.

The authored actual-fMP4 regression checks init identity, exact sample bounds,
shifted clock, shortened promise and truncated payload despite a recomputed
payload digest. Workspace/all-target compilation passed; tests remain
deferred. The actor, HTTP/peer callers, durable family metadata and shared
cache consumer pins still need integration before continuous delivery is
advertised.

### 10.14 Normal-path recipe propagation

`TranscodeOptions` now carries the explicit video sample envelope into the
existing source-bound planner and the resolved argv builder. Defaults retain
ordinary encoder behavior. Selecting the continuous video recipe normalizes
geometry and excludes source audio before resolution; it does not strip audio
from an already named muxed artifact. The authored recipe regression verifies
that this normal path and the explicit request builder resolve identical
media identities, including for an audio-bearing source.

The common High level 5.0 envelope supports its validated raster and rational
rate bounds, including qualified 1440p shapes. It does not include 4K's
macroblock frame size; that request needs a separately qualified family or
prepared replacement. The boundary regression reflects that frame-size limit.
Workspace/all-target compilation passed without unit execution.

The family actor still needs its versioned stored request and attachment
contract, creation/admission, audio pairing and routing. The normal options
carry media semantics, not a user feature readiness gate.

### 10.15 Codec buffer bound and final test accounting

The continuous High level 5.0 recipe now bounds its two-second coded-picture
buffer as well as peak bitrate. The conservative High VCL limit is 168,750
kbits, so this recipe accepts at most 84,375 kbit/s; the boundary regression
covers that exact limit. The level and profile factors come from
[FFmpeg's H.264 level table](https://www.ffmpeg.org/doxygen/4.4/h264__levels_8c_source.html).
This corrects a recipe admission bound before family creation uses it.

At final qualification, each fast-lane test needs one passing result for the
code being merged. Record individual results and rerun failures individually.
Keep passing results while repairing failures; rerun only the failed tests,
never the complete lane because one test failed. This is Paul's explicit
October 1 clarification. No unit tests run during implementation.

### 10.16 Versioned worker media roles

A continuous family worker request now carries a strict version-1 media role
and a UUID family generation through the existing stored/peer request. Ordinary
requests omit it, preserving their legacy JSON and idempotency identity. A role,
version or family change changes the request fingerprint. Older strict workers
refuse the new field; they cannot recreate a video-only recipe as muxed media.
Unknown role fields, unsupported versions, live presentation, source copy,
burned subtitles, HDR-output requests and legacy candidate contexts are refused.
Following library channels cannot accidentally become continuous VOD families.

The existing descriptor-bound VOD preparation now resolves the continuous
video role with normalized High level 5.0 video and no audio. The shared-audio
role freezes its AAC recipe and uses CPU-only resource accounting and its own
48 kHz plan. Soundtrack preparation does not require a GPU or video tone-map
proof merely because the source carries HDR. Both roles retain the existing
source attestation and executable/engine capture before naming a rendition.

Authored regressions cover strict wire parsing, distinct request identities,
unsupported-role refusal and real-source video/audio recipe preparation. Rust
1.97.1 workspace/all-target compilation passed; tests remain deferred. This makes worker creation role-aware; the family actor still
needs pairing, parent ownership, delivery routing and consumer-pin integration.

### 10.17 Actual soundtrack configuration

Shared soundtrack init publication and head regeneration now verify the actual
MP4 audio sample description against the frozen AAC recipe. The structural
parser follows the selected track to its sole `mp4a`/`esds` entry, bounds every
MPEG-4 descriptor within its parent, and verifies AAC-LC, 48 kHz, 1024-sample
frames and actual AudioSpecificConfig channel count. It refuses ambiguous
entries, extra tracks, unsupported descriptor flags, HE-AAC, PCE layouts,
short-frame modes and unknown extensions before publication. Channel count
comes from AudioSpecificConfig rather than the older container default.

The descriptor and configuration layouts are checked against FFmpeg's
[ISO-media descriptor parser](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/isom.c)
and [MPEG-4 audio configuration parser](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/mpeg4audio.c).
Authored regressions exercise actual mono/stereo encoder init segments and
malformed/configuration boundaries; unit execution remains deferred.

Inspection also found that the legacy video-boundary helper would restart an
audio-only plan at zero on every seek. Shared soundtracks now restart at their
requested AAC interval; mixed video/audio plans retain their final-video
boundary behavior. The authored regression checks both paths.

Rust 1.97.1 workspace/all-target compilation passed for the exact tree.

### 10.18 Source-bound audio/video pairing

Verified presentation families now pair actual video init facts with an actual
shared-AAC rendition. The soundtrack records its immutable rendition/init
identities and frozen recipe. Pairing requires that every video rung names
that exact soundtrack recipe and source object. Missing audio, extra audio on
a silent family, another source object or another selected-track recipe is
refused. The authored family regression covers these substitutions.

Standalone video and shared-audio workers no longer reopen an unused second
source reader: only legacy muxed encoded media needs that separate audio
input. This retains the existing attested descriptor path for each producer.

Pairing is media validation, not actor activation or advertised availability.
Parent ownership, bounded child attachment and playlist routing remain.

Pinned workspace/all-target compilation passed. Physical reservation preflight
also checks the source object retained by the verified rung directly.

### 10.19 Prepared first-frame active-wall budget

The web prepared first-frame deadline now excludes explicit viewer Pause and
resumes with the remaining active budget. Decoder pauses and stalls while Play
is wanted still spend the deadline. Transport intent updates the deadline
owner directly, including a Play whose media promise fails without a native
event. Revision fencing rejects a queued older deadline; settlement and every
teardown release the deadline closure and presentation callback.

The authored regression covers a long explicit pause, remaining-budget resume,
stale deadline, actual presented-frame settlement and a decoder pause that must
still time out. Its focused command is
`node tests/playback/web-control.test.js --prepared-first-frame`; it has not run.
Offer, overlap, server and native budgets still need their CQ6 integration.

### 10.20 Prepared rollback retains current transport intent

If the successor fails its first-frame proof, the web rollback restores the
incumbent media and session metadata while preserving the latest viewer Pause
or Play. Previously it restored the intent captured before exposure, which
could resume a paused viewer or undo a newer Play. The authored regression
covers both directions and checks that the incumbent has one restored owner.
Its focused command is `node tests/playback/web-control.test.js --prepared-rollback`;
execution remains deferred to the final fast lane.

### 10.21 Prepare immutable children without public sessions

The VOD create funnel now separates immutable rendition preparation from
public session registration. Both ordinary and cluster creation use the
extracted preparation path. It returns the existing rendition build guard,
which stays held until the caller commits its reader graph; source attestation,
recipe resolution, cache adoption and cancellation-independent build settlement
retain their prior ordering. This is the seam for parent-owned children, not
activation of continuous delivery. Pinned workspace/all-target compilation
passed; no unit execution was added during implementation.

### 10.22 Apple prepared first-frame active-wall budget

The Apple first-frame wait now spends its six-second proof budget only while
Play is requested. Transport intent updates the monotonic budget on the
Pause/Play edge, and polling never mistakes decoder stalls for viewer Pause.
The proof also rejects a detached item or replaced video output, so a queued
pixel from an old attachment cannot settle the new one. Pure regressions
cover remaining-budget resume, long Pause, active stalls, saturation and
backward clock samples; execution is deferred to the final lane.

iOS and tvOS arm64 simulator compilation passed with Xcode 27.0; this is
compile evidence, not device or physical-display qualification. The existing
item-transfer prepared exposure still needs repair and qualification.

### 10.23 Android prepared first-frame active-wall budget

Android's switched-successor deadline now preserves its remaining five-second
budget through explicit Pause and background suspension. Playback commands and
lifecycle edges update the budget directly; active decoder stalls still spend
time. The proof owner clears the budget on first frame, failure and release.
Pure regressions cover long Pause, remaining-budget resume, active stalls and
backward clock samples, with unit execution deferred.

Production and unit-test Kotlin sources compiled successfully using the
installed Android 37.0 SDK and Java 21 runtime. The pinned CI environment uses
Java 25; this local compile is not a receipt for that environment. The surface
transfer and native/device qualification remain unfinished.

### 10.24 Integrate current playback ownership repairs

Main through `3a512c890` adds terminal-owner lifetimes, captured-attachment End
reporting, measured delivery Activity and the updated seek controls. The
continuous-quality branch now integrates those changes. Two overlapping web
conflicts retain both terminal Keep waiting refusal and the manual retained
quality Retry action; the prepared harness includes both transport telemetry
and cancellation dependencies. Unit execution remains deferred. Compiler
evidence was refreshed against the merged tree: Rust workspace/all-targets,
iOS production/test sources, tvOS, and Android production/test sources passed.
No unit tests executed.

### 10.25 Verified shared-audio master construction

Verified presentation families now render a deterministic HLS master with one
shared soundtrack URI and the actual codec, channel, raster and cadence facts.
Each variant's peak bandwidth includes its video and soundtrack; an average
appears only when both component averages are known. The caller supplies
container-inclusive server budgets. Missing, duplicate or foreign budgets,
invalid averages, overflow and audio/video identity aliasing are refused.
Silent families omit the audio group and codec.

The renderer makes no independent-segment or physical presentation claim from
init facts alone. Parent ownership, admission and bounded child serving remain
required before advertising these paths. The authored family regression covers
the above cases; unit execution remains deferred. Attribute construction follows
[RFC 8216 §§4.3.4.1–4.3.4.2](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.4.1).

### 10.26 Immutable role-specific media playlists

Verified video and AAC renditions now render child VOD playlists from their
own clocks, with immutable init-artifact URLs and bounded segment paths. A
plan must have the expected role, timescale, consecutive ordinals and complete
ordinary intervals; video tails keep whole frames, while AAC retains its
existing final-packet trim. Empty/oversized plans, displaced boundaries and
understated target durations are refused. Regressions compare the rational
video interval with the separate AAC interval and exercise malformed plans.
The serving actor still needs to connect these paths to authorized parent
readers; rendering does not grant admission or advertise availability.

Pinned Rust 1.97.1 workspace/all-target compilation passed for master and
child playlist construction. Unit execution remains deferred.

### 10.27 Android rollback retains live viewer controls

A failed prepared handoff now restores the predecessor media with the current
volume and playback speed from the successor and the standing transport intent
filtered through the current lifecycle pause. It no longer restores controls
saved before the switch. The obsolete predecessor control snapshots are removed.
The existing prepared-authority regression checks these bindings; execution is
deferred until final qualification.

### 10.28 Web rollback retains live audio and speed controls

After a failed exposed successor, web rollback now copies the successor's
current mute, volume and playback speed to the retained predecessor. The
regression drives the shipped rollback with settings changed since preparation
and verifies they survive alongside standing Play/Pause intent. Script syntax
checks pass; unit execution remains deferred.

### 10.29 Shared soundtrack covers the final video frame

Continuous AAC plans and worker duration now end at the frozen video grid's
last whole frame, rounded outward across clocks by less than one audio sample.
Ordinary AAC intervals remain 94 frames; the final packet retains its trim.
This avoids a short soundtrack without adding an entire AAC interval or
changing the attested source duration. The encoded identity includes the
changed worker duration, so older shorter media cannot inherit its cache key.
The worker receives source duration to avoid rounding an already aligned tail
onto another video frame. Regressions cover rational/integer cadences and the
planner/worker duration agreement. Unit execution remains deferred.

### 10.30 Atomic bounded producer-group admission

The existing node admission pool now supports a group of up to three producer
permits in one CPU/GPU counter transition. A group fits its total allowance
without the single-job idle oversize exception. Capacity, priority or arithmetic
refusal changes no counters; each successful child permit remains independently
owned until retirement. The normal single-producer API uses the same engine
and preserves its established policy. The regression covers a two-video plus
AAC group, partial release, waiting-viewer priority, overflow and refusal.
Encoder permits now support shared ownership so a parent can retain its reservation while a worker clone remains held through reap. Ordinary producers still release with their exact process. Parent ownership still needs to retain these permits for its advertised family;
this allocator alone does not grant child serving. Unit execution is deferred.

### 10.31 Exact shared-audio dependency projection

A verified presentation family now derives AAC interval dependencies from
its immutable video/audio plans using integer cross-clock comparisons. The
projection validates both clocks and their common film end, includes both
AAC intervals when a video boundary crosses them, and refuses missing,
foreign, gapped or extended soundtracks. A silent family returns no audio
dependencies. Each video interval names at most three AAC intervals. The
regression covers boundary drift and the final whole-frame tail. This is the
server-side projection; durable AAC reservations and serving integration still
need to call it. Unit execution remains deferred.

### 10.32 Durable shared-audio reservations and verified publication

The parent ledger now retains shared AAC separately from its video
transactions, within the same total interval and byte allowance. Video
disposal, cancellation and owner takeover do not remove these facts; named
audio transport disposal does. The soundtrack identity remains frozen even after its byte pins are disposed, so the same attachment cannot substitute another audio rendition. SQLite and Hiqlite retention queries include
the AAC dependencies. Empty fields remain absent in older ledger fixtures.

The physical reservation publisher derives audio dependencies from verified
family plans and checks cached init identity, AAC-LC shape, payload bounds,
contiguous sample clocks and permitted final-packet trim. It holds all video
and audio keys through the one durable CAS settlement. Budget refusal writes
nothing. Regressions cover durable backend projection/takeover/disposal,
capacity refusal and cached AAC bytes. HTTP/peer and parent actor callers,
shared-cache pins and final qualification remain unfinished. Unit execution
is deferred.

### 10.33 Exclusive use of retained producer reservations

A retained family CPU/GPU reservation now grants one exclusive worker claim.
The real VOD generation claims it before opening inputs or launching FFmpeg,
and the producer slot holds that claim until process cleanup releases it.
Cloning the reservation preserves capacity but cannot launch a second worker
under the same credit. A duplicate launch refuses before spawning; a later
generation can claim the reservation after the previous worker is reaped.
The existing bounded producer-group regression now covers duplicate refusal,
claim reuse and capacity retained through parent teardown. Pinned compiler
checks and the normal hook apply; unit execution remains deferred until final
adversarial review. Parent integration remains outstanding.

### 10.34 Media-reader retirement and parent captions

Rendition reader retirement is now separate from ending the public playback's
subtitle window. Existing single-rendition endings still perform both in the
same order. Family children can retire their media demand and parked requests
without fencing the parent out of its next caption window. This is a lifecycle
refactor for parent integration, not a claim that family children are attached
yet. The normal Rust compiler and commit checks apply; no unit execution.

### 10.35 Exact family membership identity

The presentation-family identifier now hashes the canonical verified video
membership, each actual init identity and the exact shared soundtrack. The
video compatibility hash remains a join class; it no longer doubles as
attachment authority. Reordering the same members preserves family identity,
while replacing a rendition, init object or soundtrack changes it. The
physical reservation writer already compares the attachment family ID, so
it now refuses a different compatible set under an existing attachment. The
family regression covers these distinctions; pinned compilation applies and
unit execution remains deferred. Durable parent reconstruction still needs
the family descriptor and actor integration.

### 10.36 Parent-owned media-reader cleanup

The public VOD parent now owns private media readers and their retained
admission handles. Terminal cleanup drains them before ending the parent's
caption window; replay cannot remove another parent's later reader. A
same-ID replacement takes the outgoing child graph and retires it without
fencing the new parent's captions. Registry-committed replacement cleanup
and idle reap keep an owned lifecycle guard in a cancellation-independent
task, so a cancelled requester cannot leak children or let resurrection
race the departing graph. The committed replacement driver is kicked before
asynchronous teardown.

Authored regressions cover private child isolation, terminal replay, same-ID
replacement and cancelled idle reap with a concurrent resurrection. Pinned
Rust compilation applies; unit execution remains deferred to final review.
Family construction, admission attachment and child HTTP/peer serving are
still needed before this graph carries production continuous media.

### 10.37 Retained capacity reaches real producer starts

Each immutable rendition now weakly records the exact capacity held by its
worker. A parent can adopt that credit without counting the same physical
producer twice. Real VOD starts bind admission to the rendition and take its
exclusive worker claim; restarts borrow a still-retained parent entitlement
after reaping the predecessor. A racing duplicate admission is released.
The cache keeps only a weak link, so the final parent/worker release returns
CPU/GPU capacity rather than leaving a warm rendition permanently admitted.
The authored regression covers duplicate admission, shared parent ownership,
a cold restart, adoption during reap and expiry. This connects capacity to
the real driver; family group allocation and parent attachment still need
their caller. Pinned compiler and normal hooks apply; no unit execution.

### 10.38 Private-reader response ownership

Media lookup can now resolve a child only through its exact public parent's
owned reader graph and matching source object. Cache keys alone cannot read
another parent's media. A child response captures the private reader identity
as well as the parent incarnation; detach/rebind of the same cached rendition
invalidates the old header, status and EOF owner. Completed child delivery
advances its own reader frontier, while parent marker/presentation observations
remain independent. The authored regression exercises cross-parent refusal,
frontier isolation and stale responses after rebinding. Pinned compiler checks
apply; unit execution remains deferred. This is serving ownership integration;
the family constructor and HTTP/peer child routes remain to be connected.

### 10.39 Parent-owned child HTTP and peer media

Typed child media routes now dispatch only through the exact parent's private
reader graph, share its delivery meter and response-publication fences, and
attribute blocked demand to the private reader. Audio and continuous video
roles cannot alias each other. Init URIs require the frozen served-init SHA
and rehash the opened bytes before publication; segment ordinals are bounded
and canonical. Peer relay carries a separate strict child-resource variant
with the existing segment deadline; the legacy flat-resource path remains
unchanged. Older strict peers refuse the new variant rather than interpreting
it as another resource. The authored relay regression covers traversal,
identity aliases, role refusal and deadline ceilings. Pinned compiler and
normal hooks apply; no unit execution. Family construction, durable restoration
and playlist exposure remain unfinished, so continuous playback is not yet
advertised or qualified.

### 10.40 Parent control projects to private media clocks

Accepted nonterminal parent control now advances every owned private reader
on its own immutable plan, including AAC-only entries and a private reader
sharing the root rendition. All reader maps are acquired before sequence
acceptance; cancellation while a child map is unavailable cannot partially
accept a seek. Replays leave the graph unchanged, and later child downloads
cannot override an accepted playback anchor. End retains the existing detached
cleanup path. The existing seek regression now includes shared AAC and root
private readers, forward/backward seeks and late audio delivery. Pinned
compilation and normal hooks apply; no unit execution.

### 10.41 Child downloads share parent admission

Private video/audio downloads now spend the public parent's single blocked
GET allowance and fairness reserve, including retained requests whose bytes
arrived but whose file is not yet opened. Scheduler demand and retirement
remain attributed to the exact private reader. Activity diagnostics aggregate
the parent's child waits. The authored regression covers shared caps, another
parent's independent allowance, exact retirement, and capacity release after
notification/cancellation. The real child HTTP serving path supplies both
identities. Pinned compilation and normal hooks apply; no unit execution.

### 10.42 Verified child playlist delivery

Child playlists now have parent-capability HTTP and typed peer relay routes.
The server verifies the opened init against its frozen SHA, checks the actual
AVC or AAC sample entry against the executable recipe, and renders the exact
immutable role plan. Video from a source with sound requires one unambiguous
parent-owned AAC recipe with the same source object and selected track;
missing or conflicting children cannot produce a soundless family playlist.
Header/status/completion authorization keeps the private response owner.
Init byte verification is shared with child media delivery. The existing
relay regression now covers child playlist identity and role validation. The
existing actual AAC fixture also exercises private playlist delivery, exact
init bytes/ETag, wrong-role refusal and cross-parent refusal without creating
another media fixture campaign.
Pinned compilation and normal hooks apply; no unit execution. Parent family
construction, admission and durable restoration still precede master exposure.

### 10.43 Catalog contexts for continuous workers

Continuous workers accept trusted normalized SDR catalog contexts, including
the existing 1440p profile only on its matching rung. Legacy geometry and HDR
contexts remain incompatible with the AVC family contract. The context stays
out of worker JSON and must be reconstructed from the retained catalog
envelope. Video-only and shared-AAC production cannot publish a speed proof
for the ordinary muxed catalog recipe. The existing strict-wire regression
covers these combinations. Compiler checks and normal hooks apply; unit
execution remains deferred to final qualification. Family attachment and
client switching remain unfinished.

### 10.44 Atomic continuous video and soundtrack attachment

The normal manager now resolves shared AAC for a continuous video request
and reconstructs it during same-node durable restoration. It borrows the
selected video's end grid, independent of its CPU-only encoder recipe.
Catalog identity is checked against the original muxed plan before deriving
role-specific recipes; their work remains excluded from muxed speed proofs.

Creation prepares media independently, reacquires build gates in sorted key
order, and rechecks the exact cache objects and source fences. It reserves
video and AAC together before registering either private reader. Existing
workers contribute their exact retained credits; only missing roles consume
new capacity. The whole logical group must fit current CPU/GPU policy even
when just one missing permit is requested. Refusal or cancellation before
registration leaves no partial reader graph. Parent End, replacement and
idle cleanup retain their existing cancellation-independent retirement.
Prepared promotion now includes all owned media.

The existing AAC campaign now authors a one-credit-short refusal, successful
paired attachment with retained credits, private playlist/init delivery, and
whole-group release after End and confirmed reap. The manager planning
regression consumes an actual normalized SDR catalog candidate. Pinned
all-target compilation and normal hooks apply; unit execution is deferred.
This connects one video and one soundtrack in production; additional video
rungs, immutable master exposure, schedule transport, durable family
descriptors and client switching remain unfinished.

### 10.45 Admission refusal preserves the incumbent

Continuous parent creation skips the legacy pre-create supersession sweep.
A failed paired admission cannot End the currently healthy parent before a
replacement exists. The existing prepared/CAS owner or an explicit client
release remains responsible for retiring the old attachment after success.
The manager regression now creates an incumbent, lowers capacity to fit only
AAC, refuses the new video/audio group, and asserts that the incumbent still
serves its playlist. Synthetic fixture facts also declare their known square
pixel ratio for normalized AVC planning. Pinned compilation and normal hooks
apply; unit execution remains deferred.

### 10.46 Android manual failure retains playback

A failed manual offer or prepared successor now settles once as retained
current when the incumbent has established playback, is ready, and has no
player error. It preserves the saved preference while restoring the exact
incumbent wire selection and media recipe; a later seek cannot implicitly
apply the failed saved choice. The viewer can Retry the choice or explicitly
Apply with restart in Playback settings. The latter publishes its current
intent before routing and respects newer commands. Genuine incumbent failure
keeps the existing recovery path.

A failed exposed successor restores its predecessor without scheduling a
reopen when that player is healthy; recovery otherwise uses the latest viewer
destination, rather than the old exposure position. Existing latest transport,
volume and speed restoration remains intact. Pure regressions cover replay
settlement, stale epochs, standing-wire versus saved-preference separation,
Pause retention, later seeks, retry and stale failure receipts. Android
production and unit-test sources are compiled without unit execution. Native
warm-surface exposure and device qualification remain unfinished.

### 10.47 Apple manual failure and rollback retain the standing recipe

Apple now separates a failed saved choice from the exact incumbent wire
quality. Manual offer/preparation failure retains an established, ready,
error-free incumbent whose recipe was attached before the request. Retry and
explicit Apply with restart are available in the Quality menu on iOS/tvOS.
Apply publishes its latest intent before reopening; subsequent unrelated
media operations keep the standing quality until an explicit retry or apply.
The failed optional destination is cleared, or a separately pending viewer
seek is resumed against the retained recipe under its current owner.

A failed exposed manual successor can restore the incumbent under the same
exact-attempt fence used by Auto. An exposed item without first-frame proof
cannot be mistaken for a healthy incumbent. Latest Pause/Play and speed
remain authoritative; successful exposure also uses the preferred speed.
A superseding viewer epoch refuses old pre-exposure work. Successful manual
commit settles the recipe revision, so a later seek cannot reopen solely
because of the already completed quality change. Pure retention regressions
cover stale epochs, replay, attached-recipe eligibility and explicit retry.
iOS production/test-source and tvOS production compilation apply; unit
execution remains deferred. Warm player/layer ownership and actual device
continuity evidence still remain unfinished.


### 10.48 Autonomous companion registration

A continuous video request can name one additional catalog candidate. The
owning worker reconstructs both contexts from the same source and retained
decoder snapshot; companion contexts stay out of JSON. The companion must
be a distinct normalized SDR encode rung, and its derived immutable recipe
must share the source, soundtrack selection and video grid with the primary.
Same-node durable reconstruction now restores the catalog context first.
Standalone role fingerprints and wire omissions remain unchanged.

Creation prepares each media role independently and rechecks all cache
objects under sorted build gates. It reserves both videos and optional AAC
as one group before registering any private reader, with every foreground
credit retained for the attachment lifetime. Missing contexts, mismatched
roles and recipe aliases fail explicitly. Existing End and reap ownership
covers all three roles. The existing AAC fixture now includes a three-role
one-credit-short refusal, successful attachment, and full release after reap;
strict-wire coverage checks companion identity and context omission. Pinned
compilation and normal hooks apply; unit execution remains deferred.
Verified master exposure, controlled scheduling and client switching remain
unfinished, so continuous capability is still not advertised.


### 10.49 Verified autonomous master delivery

The existing capability-authenticated master route and typed peer relay now
resolve continuous parents from their owned reader graph. Each actual init
is opened and hash-checked before constructing the exact two-rung AVC family
and shared AAC pairing. Init waits hold no build gates; final verification
uses sorted gates and rechecks source, init identities, parent incarnation,
reader membership and retained admission before publishing. An incomplete
family cannot fall through to an ordinary single-video wrapper.

Bandwidth is a conservative server delivery ceiling, including container
bytes and the shortest planned tail. Video allows three nominal rates plus
two nominal-rate seconds of burst and 256 KiB of container allowance; AAC
allows twice the nominal rate plus 64 KiB of container allowance. Burst and
container allowances are divided by the shortest planned interval. These
are enforced on full objects before production publication and again on
cached child delivery. They are not measured averages or evidence of an
encoder's peak behavior; JIT masters omit average bandwidth. Tail-heavy
plans may advertise a large conservative ceiling and still need ABR
qualification. Oversized objects fail explicitly instead of violating the
advertised budget.

The existing AAC campaign now authors actual master and all child-playlist
verification, stable repeated master bytes, and stale-owner refusal after
End. The manager regression covers exact byte-boundary acceptance/refusal
for video and AAC. Pinned compilation and normal hooks apply; unit execution
remains deferred. Durable family descriptors, controlled scheduling and
production client integration remain unfinished.


### 10.50 Apple warm player and layer ownership

Prepared Apple playback keeps its item in its original player and attaches
that player to a muted staging layer. The inline surface promotes the
already-ready layer synchronously; it never moves the item into another
player. Layer warm-up precedes the final playhead sample and alignment, and
an alignment that falls more than 250 ms behind current playback is refused.
Promotion refuses active or starting PiP and external playback rather than
rebinding those surfaces optimistically; the healthy incumbent stays owned.
The staging player cannot claim external playback. Ordinary external playback
is restored after successful inline frame proof.

Player ownership, periodic observation, item observation, subtitles, audio
controls and current session identity move together. The muted predecessor
keeps its player and layer until proof, follows latest Pause/Play and rate,
and can be promoted back without an item transfer. Rollback fences title,
open, seek and recipe identity while preserving newer transport commands.
Explicit transport changes advance manual retention without reviving an
older quality choice. Predecessor health is sampled before entering the
transition. An absolute 12-second overlap bound releases the second pipeline
even when Pause parks the active-time frame budget.

The existing attempt-fence matrix covers the new rollback scope, and a pure
retention regression covers transport advancement and stale choices.
iOS production/test-source and tvOS production compilation apply without
unit execution. AVPlayerLayer readiness and decoded output remain software
facts; physical display and audible continuity still require device
qualification. The Android warm-surface path remains unfinished.


### 10.51 Android finite prepared overlap

Android's prepared exposure now has the same absolute 12-second dual-pipeline
limit as Apple, independently of its five seconds of active first-frame
observation. Pause and background still park observation; they cannot retain
the predecessor decoder forever while the target has no frame. The existing
rollback and retain-current owner settles expiry. A pure regression covers
paused overlap expiry, active budget preservation and backward-clock samples.
Android production/test-source compilation applies without unit execution.

Warm Android composition still needs an application-owned surface hierarchy.
The [SurfaceView contract](https://developer.android.com/reference/android/view/SurfaceView#getSurfaceControl())
forbids arbitrary transactions on its internal control; only child reparenting
is supported. Directly changing that control's visibility or stacking would
violate the platform contract. The [surface guidance](https://developer.android.com/media/media3/ui/surface)
also requires retaining SurfaceView for full-resolution TV rendering. No
unqualified conversion of all devices to TextureView has been introduced.
Connected Apple devices were inventoried for later qualification; no Android
device was connected to ADB at this point. Native device evidence remains
unfinished.


### 10.52 Catalog provenance and verified family description

Each private video reader now retains its exact catalog candidate identity.
The family verifier returns reusable init-verified media facts; the master
and independently versioned `quality-family` metadata derive from that same
verifier. The bounded metadata names exact candidate, rendition and init IDs,
actual raster/codec/cadence, container-inclusive delivery ceilings, stable
relative playlists and the honest `autonomous_reserved` resource mode. It
cannot invent a candidate for a rung whose provenance was not retained.

The read-only capability route has a typed peer relay and existing response
ownership fences. Ordinary parents answer family absence under their own
response owner; a missing local owner uses bounded reconstruction and keeps
terminal/owner-loss refusals. Legacy start and control JSON remain unchanged.
The existing AAC campaign covers candidate mapping, two verified video rows
and the exact shared soundtrack description. Pinned compilation and normal
hooks apply; unit execution remains deferred. Schedule mutations, durable
family descriptors and production client adoption remain unfinished.


### 10.53 Owner-bound schedule delivery

An independent bounded `quality-schedule` POST and exact-write-authenticated
peer endpoint now connect the durable ledger to the production VOD owner.
The autonomous family remains fully reserved for its attachment; Prepare
creates no extra producer. It records Preparing before waiting, materializes
at most two exact video entries and their shared AAC dependencies without
holding build gates, and verifies actual init, payload hashes and sample
intervals. A preparation refusal retains current playback. Canonical command
receipts remain separate from the current ledger snapshot.

The original append frontier and preparation deadline are durable and cannot
be moved or extended by replay. A fixed 64-owner pool bounds detached cleanup
ownership across HTTP disconnects. Later exchanges settle expired Preparing
rows; cancelled or superseded preparation cannot publish Ready. Scheduled
writes invoke the physical reservation publisher and derive shared AAC pins
in the same CAS. Exact parent lifetime and build guards survive any submitted
Store write until its settlement. Disposal and completed append facts can
still reduce existing reservations after a source or producer failure.

Targeted regressions cover immutable frontiers, owner refusal, HTTP body and
peer-auth bounds, and the actual autonomous campaign from preparation through
shared reservations, lost-append cancellation and named disposal. The correct
campaign name is `shared_audio_vod_reserves_cpu_only_and_publishes_one_audio_track`;
this is the regression for the family metadata added in §10.52 as well.
Pinned all-target compilation and normal hooks apply without unit execution.
This delivers the autonomous-family writer, not controlled cold-rung admission
or production client switching. Durable family reconstruction, shared-consumer
accounting and native qualification remain unfinished.


### 10.54 Independent family creation and common text renditions

Authenticated clients can now request a family through the separate versioned
`/files/{id}/hls/continuous-sessions` route. Its nested ordinary start uses the
existing recipe planner, source/capability validation, durable activation,
placement and atomic three-role admission. Both exact normalized SDR encode
candidates must belong to the same current worker catalog; Original, copy,
HDR and burned-subtitle recipes refuse this incompatible media contract.
Explicit family creation does not depend on a readiness setting or enable a
second Auto policy. Ordinary and Library-channel start JSON remain unchanged;
a missing route on an older server cannot silently discard the family ask.

The response points to the actual multivariant master rather than the silent
video-only root playlist. That master now reuses the existing native text
rendition rules, with one common subtitle group on every verified video rung.
The transform retains exact video/audio identities, budgets, codecs and
rasters; source metadata never substitutes for those facts. Initial native
selection remains in the master query. Caption and strict-envelope regressions
are authored. Pinned compilation and normal hooks apply without unit execution.
Production client adoption, controlled cold-rung behavior, durable family
reconstruction and native qualification remain unfinished.


### 10.55 Rolling readiness for one selection

The autonomous schedule envelope can now request a bounded next append window
for an existing wanted transaction. It verifies the next actual video/AAC
samples and publishes owner readiness without inventing another selection,
advancing the command sequence, or changing scheduled dependencies. A window
cannot also carry a selection command. Every scheduling operation still runs
through physical verification and durable reservation publication. Cancellation,
supersession and owner changes reject late window results.

The real autonomous campaign covers advancing readiness after reservation,
preserving the original intent/sequence and pins, and refusing a renewed window
after lost-append cancellation. Source and normal-hook compilation apply;
unit execution remains deferred. Production append acknowledgements and
presentation integration remain unfinished.


### 10.56 Late transport facts after End

End does not prove that a client's buffered fragments were disposed. The
schedule route now accepts bounded late completed append, presentation,
cancellation and named disposal facts under the exact terminal generation,
owner epoch and attachment. It cannot Prepare, renew readiness or schedule
new media. A distinct SQLite/Hiqlite CAS reducer applies only those operations
and compares the old canonical ledger JSON as well as its revision: even a
forged snapshot cannot introduce terminal dependencies. Ordinary active
reservation writes remain refused after End.

The existing backend contract covers End winning before shared AAC disposal,
wrong-owner refusal, forged-snapshot refusal, exact replay and stale revision.
This closes the acknowledgement race without interpreting End as absence of
client media. Pinned compilation and normal hooks apply without unit execution.
Client disposal and presentation acknowledgements remain unfinished.


### 10.57 Independent client bootstrap

The dedicated family creation response now wraps ordinary playback with the
quality protocol's own durable parent generation, current owner epoch and
schedule/metadata endpoints. It reads the active durable route after
idempotent creation, with bounded storage lookup, and rejects expired, ended
or malformed ownership. This negotiation does not depend on optional legacy
M1 control. Ordinary and Library-channel response JSON remain unchanged.
Retries retain the original start identity when the bootstrap read fails.

The ownership regression covers a route without legacy control, exact endpoint
binding, terminal/expired ownership and JavaScript-unsafe epochs. Source and
normal-hook compilation apply; unit execution remains deferred. Production
web append and presentation integration is the next step.


### 10.58 Production sample provenance inspection

The served web app now includes a bounded, single-track AVC/AAC fMP4 inspector
for the verified family. It reads exact sample clocks, composition offsets,
lengths and payload locations; checks init clocks/raster/configuration, mdat
bounds, overlapping payloads and safe integers; and hashes sample lengths
plus elementary bytes independently of container headers. Invalid parses
cannot replace retained init context. Payload, box and sample counts are
bounded and overlap verification sorts offsets rather than scanning every
earlier sample. This is the provenance helper for the production adapter; it
does not install callbacks or alter playback by itself.

Synthetic regressions cover real sample boundaries, container header changes,
changed elementary payload, signed composition, truncated/out-of-mdat data,
and failed-init context isolation. Syntax and source compilation apply; unit
execution remains deferred. The loader and completed SourceBuffer receipts
still need wiring.


### 10.59 Bounded web schedule protocol

The served continuous protocol helper validates exact family membership,
clocks, identities, bounded ledger dependencies and canonical receipts.
Responses are read as bounded streams under an absolute request timer.
Mutations are serialized; lost acknowledgements retry the identical sequence,
transaction and operation before any newer request can run. A failed pending
exchange remains owned rather than turning uncertainty into cancellation or
absence. Inputs are copied when queued so later caller mutation cannot change
an outstanding command. The queue itself is bounded.

Regressions cover a lost receipt followed by newer work, immutable queued
commands, contradictory append facts and wrong attachment/owner responses.
Syntax and compiler checks apply without unit execution. Loader, completed
append, disposal and decoded-frame observers remain in progress.


### 10.60 Production web family enrollment and append ownership

Cold compatible hls.js starts now negotiate a read-only catalog and a
dedicated family session. The catalog reuses existing worker recipes and pairs
only decoder-compatible normalized SDR encodes from one worker, with distinct
actual rasters. It does not depend on the existing Auto setting or choose an
Auto policy. The existing selected candidate or height owns the primary; the
companion is a bounded adjacent family member. Ordinary, Original, native HLS
and Library-channel paths retain their existing contracts. Unsupported route
responses fall back to ordinary creation.

The hls.js fragment loader withholds progressive bytes and success until init
identity, actual sample clocks/payload/configuration and durable reservation
match. Rolling owner readiness extends one choice. Quality changes select
future video loads on the same player; shared audio stays attached. Completed
SourceBuffer append and removal observations are captured before hls.js can
start its next queued operation. Facts run in completion order even when
hashing or acknowledgements lag. Only actual decoded-frame callbacks with
matching raster and sample time publish presentation, separately from asking
or appending. MediaSource transfer is explicitly not a disposal barrier.
Unexposed aborted video reservations can settle; exposed media retains its
owner until completed removal or full detach.

Compatible continuous control snapshots decline prepared replacement while
retaining ordinary transport/recovery actions. Other recipe changes still
use the prepared path. Failed optional readiness restores future incumbent
scheduling while retaining older media facts. LAN HTTP clients have a bounded
software SHA-256 fallback; sample fingerprints include each timestamp,
duration, length, elementary payload and decoder configuration.

Regressions are authored for withheld loader data, append-versus-presentation,
failed append, same-player quality selection, detach/transfer distinction,
cryptographic fallback and action negotiation. Pinned all-target compilation
and normal hooks apply; unit execution stays deferred. Browser/media-device
qualification, lost-ack reconciliation across End, controlled cold-rung
scheduling, shared-cache consumer accounting and native adapters remain
unfinished. This implements the autonomous two-rung web path in source; it
does not claim physical audiovisual qualification.


### 10.61 Reconcile lost reservations under durable End

The schedule route now permits a bounded read of an existing ledger under
the exact terminal parent, owner epoch and client attachment. Its explicit
terminal marker proves durable End; the read neither creates a ledger nor
reduces any dependency. Active responses omit this additive marker. Late
Prepare, window and Scheduled operations remain refused.

After actual MediaSource detach, the web adapter first recovers any pending
canonical acknowledgement. If End has overtaken it, a validated terminal
snapshot discovers the reservations that actually landed. Cleanup commands
use a sequence above both durable acceptance and the uncertain request, so
a late fact cannot overtake disposal. An active snapshot cannot clear the
uncertainty. Named video and shared AAC disposal then uses the late-fact path
from §10.56. Owner changes still refuse stale cleanup; they are not absence
evidence. Regressions cover lost reservation acknowledgement, false active
proof, read-only retained pins, wrong attachment and refused post-End scheduling.
Pinned source and normal-hook compilation apply without unit execution.


### 10.62 Sustained receipts fit the existing bounds

Canonical acknowledgements now retain accepted transaction metadata, while
current media facts live once in the ledger. Durable replay stores the exact
operation and accepted metadata with the ledger's fenced identity, rather
than duplicating identity and accumulated media arrays per command. Exact
replay after later disposal still returns the original acknowledgement;
changed operations at the same sequence still conflict. Epoch adoption clears
replay authority as before. Clients read current pins from the ledger.

The web adapter reports the first presented frame once per transaction and
batches subsequent actual completed video appends in pairs. Removal facts
are captured at updateend, then sent in batches of four or after ten seconds.
Pending append facts settle before disposal. A seek reloading an already
removed object and a quality change flush pending evidence before scheduling
new bytes. Detach uses its actual absence barrier and clears pending timers.
No timestamp, raster, append or AAC disposal evidence is inferred by batching.

A regression models 300 rolling four-second windows with video and AAC,
durable roundtrips, canonical lost acknowledgements and conflicting replays
under the unchanged 90-second, 128-receipt and 128 KiB bounds. Web regressions
cover batching, first presentation and disposal-before-reload ordering.
Pinned compilation and normal hooks apply; these tests are authored and
remain deferred until the final review and fast lane.


### 10.63 Android keeps application-owned warm video outputs

Android API 35 and newer now stage the successor on its own SurfaceControl
child beneath the existing PlayerView SurfaceView. The SurfaceView's control
is used only as a parent; visibility, geometry and lifetime changes apply to
application-owned children. PlayerView receives a presentation delegate for
tracks and cues, so changing its player does not rebind either decoder output.
The two-output bound applies across preparation, promotion and rollback.

Actual output-specific rendered-frame evidence and sample time must match
the parked rendezvous before promotion. A visibility transaction's presented
callback then settles the warm commit. A hidden rendered frame, an old seek,
a removed output, or a later rollback cannot satisfy that receipt. The
predecessor keeps its Surface until settlement; rollback changes visibility
on the retained output. Sample pixel aspect ratio survives geometry changes.
Surface destruction drains owned controls, clears decoder bindings and
invalidates outstanding exposure receipts. Release and failed preparation
also drain owned controls. Existing pause, transport and finite physical
12-second overlap ownership still apply.

The platform boundary is an actual API dependency, not qualification status:
[transaction completion](https://developer.android.com/reference/android/view/SurfaceControl.Transaction#addTransactionCompletedListener(java.util.concurrent.Executor,%20java.util.function.Consumer))
reports presentation starting at API 35. A committed callback only reports
readiness for presentation. Older systems retain their ordinary handoff;
this implementation makes no continuity claim for that path. The
[SurfaceView parent contract](https://developer.android.com/reference/android/view/SurfaceView#getSurfaceControl())
forbids mutating the SurfaceView's own control. Physical screen, audio,
HDR/tunneling, PiP and older-Android qualification remain open.

Production and regression-source compilation passed. Authored receipt
regressions cover hidden rendering, rollback overtaking exposure, seek and
surface recreation, stale callbacks and the two-output bound. Unit execution
remains deferred until final main-readiness review and the fast lane.


### 10.64 Shared cache dependencies keep each consumer's authority

The existing SQLite/replicated query already deduplicates physical interval
JSON while retaining each parent's separate ledger. Typed dependency decoding
now also deduplicates equivalent field order and refuses contradictory facts
for one immutable artifact. An unknown or conflicting dependency remains a
retention refusal, never permission to evict. No consumer's disposal can
release another consumer's reservation.

A cross-backend regression publishes two independent parent video/AAC ledgers,
checks one physical dependency per artifact, disposes each consumer in turn,
and verifies the shared bytes stay reserved until both settle. The existing
private reader and retained-admission ownership remains in place. Physical
consumer accounting is implemented; pressure and lifecycle qualification
still remain. A typed decoder regression covers duplicate and contradictory
artifact facts. Pinned all-target compilation applies without unit execution.

Android warm exposure additionally checks a valid present fence without
blocking the application looper, closes every acquired fence, and bounds
pending observation by the same physical overlap allowance. An invalid fence
uses the platform's completed-transaction receipt on devices that do not
supply present fences. No committed-only callback is accepted as presentation.


### 10.65 Verified family proof survives durable parent restoration

Before master or family metadata exposure, the owner now binds a strict
32 KiB-bounded description into the existing durable parent recipe. It names
the exact candidate/rendition/init mapping, raster, AVC/AAC codecs, rational
grid, playlists and conservative delivery ceilings. The Store write requires
that exact active generation, owner epoch and unexpired lease, and that both
advertised candidates match the recipe's primary and companion. An existing
description can replay exactly; it can never be replaced by different media.
This uses the existing recipe column and requires no schema migration.

Source, capabilities, intent and worker recipes remain unchanged JSON values.
The proof does not renew playback or allocate capacity. It is omitted from
ordinary requests, does not change intent fingerprints, and is removed from
private companion/AAC worker recipes. Restored parent requests retain it and
rebuild catalog contexts as before; actual verified exposure must reproduce
the same proof. Changed init, membership or delivery facts refuse exposure
instead of reinterpreting the old family authority. End and owner changes
fence the write. Header/body ownership still revalidates the response owner.

A cross-backend regression covers owner/epoch refusal, exact replay, changed
init and candidate refusal, durable roundtrip, source/intent preservation and
post-End refusal. Pinned all-target compilation includes both Store backends.
Unit execution remains deferred. Controlled cold-rung admission and native
continuous adapters remain unfinished; this closes the autonomous family's
durable description path, not physical-media qualification.


### 10.66 Integrate current main without reusing shipped migration numbers

Main through `0f71852bc` is integrated. Its result lookup, preparation index
and Dolby Vision request provenance migrations retain their published order.
The unpublished quality cancellation and continuous ledger migrations follow
as SQLite v91/v92 and replicated v69/v70. The migration-chain assertions name
both new predecessors, and independent web control regressions are retained.
Pinned workspace/all-target compilation checks the integrated source; unit
execution remains deferred to final review and the fast lane. Controlled and
native integration and physical qualification remain unfinished.


### 10.67 Production continuity evidence belongs to playback-lab

The existing `scripts/playback-lab` now has a `continuous` suite. It exercises
the shipped player and daemon, requires VOD and one session, and records weak
identities for the actual element, Hls, MediaSource and video/audio buffers.
Completed buffer removals are observed at updateend, with error/abort refusal;
normal eviction behind the playhead is allowed, while removal of future media
or a replaced buffer fails the continuous contract. Truncated removal evidence
also fails. The target needs a fresh durable Presented receipt inside an
actual appended interval and the independently decoded target raster.

The suite performs twenty alternating 720/1080 selections. Its opt-in
25-minute SDR fixture leaves room for the existing sixty-second forward buffer;
qualification does not shorten that buffer or flush it to make switches look
fast. Each selection may wait up to ninety seconds for future media. Existing
video-gap, clock, runway and reopen scoring still applies. Shared audio-buffer
identity is recorded, but physical audio output remains a separate evidence
requirement. The prototype remains historical feasibility evidence.

```bash
scripts/playback-lab run --suite continuous --browser chrome \
  --server target/debug/plurxd --json target/playback-lab/reports/continuous-chrome.json
# Repeat with Firefox; retain Safari/native API limitations separately.
```

The suite and observer/scorer regressions are authored and syntax-checked;
no unit execution or twenty-switch production qualification is claimed yet.
Controlled cold-rung ownership and native adapters remain unfinished.


### 10.68 Apple warm promotion verifies the post-seek decoded sample

A hidden AVPlayerLayer's readiness can describe the frame from before an
alignment seek. Apple preparation now attaches an item-specific video output
when constructing the successor. After alignment, promotion requires a real
pixel buffer with a finite item-local display time within 250 ms of the
rendezvous and a nonempty raster, under the same item/player/viewer lifecycle
fences. The incumbent stays visible throughout this bounded check.

Warm promotion retains that exact video output for the successor's ordinary
seek and first-frame observation; it does not install a new decoder sample
observer at exposure. Discard removes the staged output and its delegate.
Stale samples, failed seeks, backgrounding and ownership changes retain the
incumbent. This improves the pre-exposure proof; it does not turn pixel-buffer
availability into a physical-display or audio-output acknowledgement.

Production and test-source iOS compilation and production tvOS compilation
passed. The authored rendezvous regression covers exact/tolerance-edge times,
stale/nonfinite timestamps and missing rasters. Unit execution remains
deferred. Device audiovisual qualification and native continuous adapters
remain open.


### 10.69 Production startup exposes the replicated placeholder contract

The isolated Chrome production probe completed fixture indexing but could not
create playback: the maintenance query introduced `$6` before `$5`, which the
replicated Store correctly refuses before I/O. This is a failed integration
receipt, not continuity evidence. The same inspection found first-appearance
ordering mistakes in cancellation settlement and verified family binding.

All three statements now introduce placeholders in order with their bindings
reordered accordingly. The common family statement also uses that binding
order in SQLite, where `$N` is a named parameter rather than a numeric index.
The existing placeholder census now includes the shared family, cancellation
and ledger SQL constants so those statements cannot escape the final fast
lane. Owner, time, cancellation and immutable media predicates are preserved.

Pinned compilation and the normal hook apply before repeating the isolated
handoff probe. Unit execution remains deferred. Retain the failed production
receipt at `target/playback-lab/reports/continuous-chrome-partial-before-sql.json`; it
contains no successful playback or native qualification claim.


### 10.70 Terminal shared-ledger SQL uses the same binding contract

The follow-through inspection of the newly censused shared constants found
one additional named-parameter ordering mistake in the post-End ledger write.
That statement and both Store callers now bind owner, epoch, revision, JSON,
time, generation, attachment and old JSON in their first-appearance order.
The exact prior-ledger comparison and terminal owner predicates remain intact;
this cannot create a reservation after End. Existing cross-backend late-fact
regressions and the expanded placeholder census cover the landing candidate.
Pinned all-target compilation applies without executing the unit lane.


### 10.71 Isolated Linux runtime for production browser qualification

The repeated macOS probe completed fixture indexing but refused normalized
continuous preparation because production decoder identity deliberately accepts
only a self-contained Linux ELF. The macOS FFprobe is not that artifact; no
identity or geometry check was relaxed. The failed receipt is retained as
`target/playback-lab/reports/continuous-chrome-partial-macos-probe.json`.

The documented deploy key authorizes `pjunod` on nuc3. An isolated temporary
source extraction there receives `git archive` of committed `cd9bb38ad`, never
Git history or repository credentials. Its verified compiler is Rust 1.97.1
`8bab26f4f`. A static FFprobe is copied from the existing daemon image through
a stopped disposable container; no running service or production data changes.
The build uses two jobs and no debug information to bound temporary resources.
The synthetic 210-second fixture supports an initial two-handoff investigation
only; the full twenty-switch and physical audiovisual qualification remain open.

The existing playback lab remains the acceptance oracle. A temporary launcher
uses its server dependency injection to own a remote daemon, isolated data and
three ephemeral listeners, with Chrome on the Mac connected through an SSH
tunnel. Its bootstrap uses the same setup, scan and index API sequence. No
unit suite has run; final review and the single-pass fast lane remain deferred.


### 10.72 Production AVC emits its required explicit color record

The isolated Linux probe indexed the fixture and created the continuous parent,
but family verification refused the AVC init because FFmpeg inherited missing
source color tags. The continuous envelope already promises limited-range
BT.709 SDR and verifies that exact `nclx` record; its VOD encoder now explicitly
sets primaries, transfer, matrix and range on the output. Ordinary VOD recipes
retain their existing arguments. This sets output signaling, not a new color
conversion or evidence for arbitrary source color layouts.

The continuous semantic fingerprint advances to `continuous-avc-high50-bt709-v2`
so old material cannot share the changed immutable encode recipe. The existing
VOD argument regression now requires all four scoped output values and their
absence from the ordinary recipe. The strict media verifier is unchanged.
The failed Linux receipt remains `continuous-chrome-partial-linux.json`; it
contains no successful continuity claim. Source compilation and the tracked
hook precede a new exact-source production probe; unit execution is deferred.


### 10.73 Empty-moov color metadata needs frame values and explicit emission

The exact v2 production probe still failed the strict color check. Inspection
of its actual immutable init bytes confirmed the box was absent. A bounded
one-second Linux FFmpeg 8.0.1 experiment separated three cases: output codec
color flags alone omitted `colr`; adding `write_colr` emitted unspecified
primaries/transfer (`2/2/1`); adding explicit output-frame `setparams` produced
the required limited-range BT.709 `nclx` (`1/1/1`, range flag zero).

Continuous VOD now applies both the frame metadata and the muxer presence flag,
with the semantic fingerprint advanced to `continuous-avc-high50-bt709-colr-v3`.
Ordinary VOD arguments are unchanged. This is explicit signaling for the
existing SDR envelope, not qualification of other color layouts. The authored
regression requires frame values and box emission together. The failed v2
production receipt is retained as `continuous-chrome-partial-linux-color.json`.
No unit execution or successful continuity claim follows from this experiment.


### 10.74 Controlled families reacquire cold video work under the parent

The continuous create route now distinguishes an explicitly controlled loader
from an autonomous engine. hls.js requests the controlled mode. The durable
family descriptor retains its exact immutable two-rung graph and init proof;
a cold video child may release its own admission without withdrawing a URI
or changing family identity. Autonomous masters retain all advertised credits.

Parent control readers and cold child readers keep their identities but create
neither background encode demand nor eviction windows. Accepted control still
updates them, and another viewer's active reader continues to drive shared
work. A selected cold target reacquires only its video and existing AAC group
under sorted media gates, the parent lifecycle and the original preparation
deadline. Optional cold admission is speculative. It restores the same child
reader before actual two-segment readiness and durable reservation publication.

Pending preparation keeps incumbent demand. Once the latest noncancelled
Scheduled promise accepts future loading, old producer demand is released
while immutable child identity, loaded media and committed Store pins remain.
The worker independently retains its credit until confirmed reap; another
active reader keeps shared work alive. Settlement rereads the durable ledger
under the lifecycle so a stale response cannot retire a newer accepted intent.
This source implementation still needs measured cleanup, pressure, cold seek
and cancellation qualification; no physical timing result is claimed.

The v3 color probe passed family verification and attached to the shipped
continuous master, then failed before a presented frame with a JavaScript
stack-overflow observation. Its failed receipt is retained as
`continuous-chrome-partial-linux-colr.json`. A repeated debugger-assisted probe
retained the same failure but captured no exception pause. The existing lab
now retains the adapter's bounded error stack to locate the actual recursive
call without changing scoring or publishing credentials. The controlled
regressions cover passive-reader fairness, future-load retirement without
disposing incumbent facts, and strict metadata in both delivery modes. Their
source is compiled; execution remains deferred to the final fast lane.


### 10.75 Normal buffering fits the interval bound

The ordinary web buffer policy retains up to 60 seconds ahead and 30 seconds
behind. With independent two-second video and AAC artifacts this is about 90
physical intervals, before a pending join. The previous 64-interval limit
could refuse ordinary healthy playback even when the byte budget was ample.
The common ledger, web receipt validator and provenance map now allow 128
intervals. The 256 MiB pin ceiling, bounded encoded ledger, transaction count
and receipt horizon remain in force; buffer lengths are unchanged.

Authored Rust and web regressions retain 45 video and 45 audio intervals and
require over-capacity refusal without losing live pins. They await the final
fast lane. The exact-source production probe of the preceding committed
controlled implementation is running separately; it remains partial evidence.


### 10.76 hls.js loader destruction does not emit another abort

The exact controlled production probe captured the first-frame RangeError:
our loader's destruction called abort, whose hls.js callback reset and
redestroyed that same loader recursively. Destruction now marks the wrapper
closed and calls the base loader's silent destruction directly. Explicit
abort is idempotent and still sends its single transport notification.

The authored adapter regression models hls.js abort callback re-entry and
requires one abort, one destruction, and silent normal destruction. The
failed production receipt is `continuous-chrome-partial-linux-controlled.json`.
Its stack identifies the cause; it does not qualify first-frame playback. The
fixed committed candidate will receive a fresh isolated production probe.


### 10.77 Outgoing callbacks cannot authorize against the target intent

The committed loader fix produced the first real continuous frame. The first
quality selection then retained an outgoing fragment callback after its loader
was aborted; authorization ran against the newer wanted rung and reported an
unreserved superseded load. Both callback entry and its serialized work now
check the loader's closure before inspecting or reserving bytes. Aborted work
cannot publish an error into the current stream. Future level selection is
installed before abort callbacks can synchronously retry outgoing work.
The authored regression requires outgoing payload privacy and no later
protocol request or current-stream error.

That partial probe remains failed as `continuous-chrome-partial-linux-loader.json`.
It also exposed the lab's dependence on a bounded console tail for counting
session births. Qualification now reads the durable VOD lifecycle stream,
scoped by exact file identity and case start. Replacement attachments count
even if the public session identity repeats; a truncated or unavailable
stream fails qualification. The census regression preserves large file IDs
and rejects incomplete evidence. The normal-buffer 25-minute fixture was
generated once on Linux with two encoder threads. Firefox 157.0 and signed
geckodriver 0.37.1 are staged in an owned temporary folder for qualification.
Unit execution remains deferred until the final adversarial review.


### 10.78 AAC starts select a window that actually covers them

A bounded debugger replay captured the missing pins directly: an AAC fragment
started at tick 192512 on its 48000 Hz clock, while the reused video readiness
ended at tick 96 on its 24 Hz clock. Selecting the preceding video entry let
that old window satisfy the membership check without covering the requested
audio. The adapter now selects the video entry containing the actual AAC start.
The owner's existing two-entry rational projection supplies all crossing audio
artifacts; only its returned immutable hash permits delivery.

The authored regression loads a separate AAC init and an AAC fragment whose
start lies just beyond the old video window. It requires a new forward window
and the real audio pin before payload delivery. The original failed full run
is `continuous-chrome-full-linux-race.json`; its durable census correctly
counted three attachments after recovery. The diagnostic replay and bounded
interval captures are retained separately and cannot qualify continuity.

### 10.79 Native overlap keeps the original pipeline construction clock

Apple and Android no longer restart the 12-second physical overlap budget at
exposure. Both first-frame budgets carry the successor's original construction
clock, including time spent preparing and aligning. Pause still parks active
frame observation but consumes physical overlap. Android checks the deadline
before rendezvous exposure; Apple bounds both its initial staging seek and
commit alignment by the remaining overlap, cancels timed-out pending seeks,
and checks the same remainder while obtaining decoded post-seek output.

Authored regressions start exposure after 11 seconds of preparation and require
only one remaining second even under Pause; an already exhausted deadline stays
exhausted. iOS production and test sources, tvOS production, and Android
production and test sources compile. No unit execution or physical device
qualification is claimed. The public Apple SDK and official documentation
([variant preferences](https://developer.apple.com/documentation/avfoundation/avplayeritem/variantpreferences)
and [maximum resolution](https://developer.apple.com/documentation/avfoundation/avplayeritem/preferredmaximumresolution))
still expose preferences rather than exact manual variant selection; the existing warm prepared path preserves exact
manual semantics. [Android's exact track overrides](https://developer.android.com/media/media3/exoplayer/track-selection)
need matching runtime groups and device adaptation evidence before a continuous adapter can claim support.


### 10.80 Current main test fixes integrate without changing production behavior

Main advanced to `c63859963`: concurrent pretranscode claims now select by job
identity, cleanup-gate assertions no longer depend on scheduler order, and the
rolling producer fixture accounts for removed task/timer ownership. These
three test files merged cleanly into this branch. The combined pinned all-target
source compilation passed; no unit suite ran.

The production Chrome replay currently uses the preceding committed candidate
`6309c5d70` and remains diagnostic until its series completes. Although this
integration changes no production source, final qualification must identify the
actual landing tree. The standing user workflow defers unit execution until
main readiness and adversarial review, retains each passing fast-lane test,
and reruns only failed tests. That overrides the older document's whole-suite
retry convention; no passing qualification receipt is fabricated for a new tree.


### 10.81 Logical fragment abort releases hls.js loading state

The AAC-corrected replay no longer reported missing pins but stalled after
selection. Inspection of the vendored hls.js abort handling identified a
separate boundary: it resets fragment loading only when `stats.aborted` is
true. The underlying XHR sets that flag only while its network request is
unfinished. Our wrapper can still be waiting on durable authorization after
the XHR reaches done, so aborting that logical fragment emitted an abort
callback with false network stats and left the controller in `FRAG_LOADING`.

Explicit wrapper abort now marks the logical stats before delegating the
transport notification. Normal successful destruction stays silent and does
not mark successful fragments aborted. The authored regression queues a
completed network payload behind selection and models hls.js's stats-based
loading reset, requiring Idle and no outgoing exposure or current error.
The failed receipt is `continuous-chrome-full-linux-aac.json`; it is retained
without a continuity claim. Final replay will use the current integrated tree.


### 10.82 Reservation bursts have independent bounded admission

The exact `6462b41e3` diagnostic replay captured HTTP 429 before its first
selection completed: reservation windows, Scheduled, Appended and Presented
facts shared the eight-per-second manual control budget. Immediate retries
hit the same window; loading stopped and recovery replaced the attachment.
The retained receipt and ledger capture are failed diagnostic evidence.

Quality scheduling now has a separate 32-per-second session budget, a
512-per-second global budget and the existing bounded map and 64-owner pool.
Admission precedes route lookup. Manual controls retain their own eight-request
budget. Rate refusal carries Retry-After; the client waits up to one second
before its one identical retry, preserving sequence and pending ownership.
Authored regressions cover isolated budgets, global spray refusal and the
ordered delayed retry. They await the final fast lane; no unit execution is
claimed. Browser replay remains required on the committed candidate.


### 10.83 Native readiness and track loading cannot extend physical overlap

Android's pre-metadata poll still allowed the old 20-second readiness limit.
It now uses the earlier physical limit and arms an item-bound deadline job
at construction, independently of metadata callbacks or the one-second poll.
Release and promotion cancel that job; exposure inherits the original clock.

Apple's post-promotion audio/subtitle asset loading could suspend before the
first-frame budget check. Reconciliation now waits only for the original
remaining overlap, cancels its task and routes timeout through existing warm
rollback. Late cancellation-ignoring asset results cannot apply tracks or
start the next reconciliation stage. The authored ownership regression
suspends audio preparation, cancels it, then releases its late result and
requires zero audio commits or subtitle preparations. iOS production and test
sources and Android production and test sources compile; unit execution and
physical qualification remain deferred.

The status page was condensed to current milestone rows. Historical CQ0
mechanics remain explicitly separate from failed production replay receipts.


### 10.84 Continuous quality does not dispatch a legacy prepared successor

The local-client diagnostic checked installation at startup but login then
reloaded and discarded that replacement. Its actual playback therefore used
the existing `6462b41e3` server and client. Both selections reached actual target
presentation on the same element, Hls instance, MediaSource and buffers with
no removals. The run remains failed: its first gap was 516.7 ms and the durable
census counted two VOD attachments. This older-source run cannot qualify the
landing tree. Both attempted local replacements remain failed diagnostic
evidence; neither proves the new delayed retry. The diagnostic now reinstalls
the client after every reload and records active-code identity in each sample.

Read-only inspection of the isolated database confirmed the extra attachment
was an active ordinary recipe with one legacy preparation row, alongside the
controlled family parent. Ordinary control's SelectionChange dispatcher now
recognizes verified controlled-family video intent and leaves it to the
rendition ledger. Other recipe axes, out-of-family choices and planned
relocation retain their existing owner. The authored route regression requires
no staging response, pending candidate or durable prepared successor, while
out-of-family height and audio changes retain the legacy path.

Apple's final layer-readiness wait also uses the original remaining overlap,
rather than another fresh four-second allowance. Pinned all-target source
compilation and iOS production/test compilation passed. Exact-tree replay
remains required. Source transfer to nuc3 is pending explicit destination
authorization requested after automatic approval review rejected it; no new
private source was sent there. Unit execution stays deferred until final review.


### 10.85 Final handoff alignment uses successor frame cadence

Web exposure now compares the decoded incumbent and successor media PTS in
film time, projecting to the incumbent's expected display time using the
actual playback rate. Adjacent callback PTS and presented-frame counts derive
one frame's duration even when callbacks skip frames. Missing cadence,
non-advancing timestamps or differing player rates cannot establish alignment.
The earlier 250 ms seek heuristic remains a coarse preparation aid; it no
longer earns final exposure. A regression covers 24 fps, skipped callbacks,
a stale picture inside the old quarter-second allowance and rate projection.

Apple loads the actual successor asset track's minimum frame duration within
the original physical overlap deadline. Decoded post-seek alignment, incumbent
position revalidation and first-frame acceptance use that interval. Unknown
cadence retains the incumbent; it does not invent a frame rate. Android's
owned warm output records frame duration from the rendered Media3 format and
compares its frame PTS with the film rendezvous mapped into successor time,
rather than checking against the successor's own playhead with 250 ms slack.
Unknown format cadence cannot establish alignment. Raster readiness remains
separate from the frame alignment receipt.

Pinned iOS production and test-source compilation, tvOS production compilation
and Android production and test-source compilation passed. JavaScript syntax
passed. The focused regressions are authored and unexecuted, following the
user's final-review-first fast lane. These changes do not establish physical
display or audio continuity. Apple's incumbent comparison still uses its item
clock; Android's older single-surface fallback and the native continuous
adapter still require their remaining work and device qualification.

### 10.86 Diagnostic asset identity survives navigation

The attempted reinstall-after-reload diagnostic also sampled the older retry
implementation during playback: CDP's reload returned while the old document
was still complete, so replacement could precede the actual navigation. Its
failed receipt is retained with an explicit source-attribution correction.
The diagnostic now fulfills only its owned Chrome tab's continuous-quality
script request with the local committed asset. No repository source goes to
the Linux host. All 133 sampled states in this run identified the delayed
retry code as active, with one intercepted asset response.

This mixed-source run uses the unchanged `6462b41e3` Linux daemon and the local
`be001d003` client asset. It remains failed: the first target presented, the
second selection did not commit, the durable census counted two attachments
and a final schedule request returned 503. It does not qualify the isolated
rate-budget server fix or the legacy-dispatch ownership correction. Exact
candidate replay remains pending the requested source-transfer authorization.
Earlier attempted local-client receipts preserve their original metrics and
claims, with the correction recorded alongside them rather than becoming
successful evidence.


### 10.87 Apple parks at a future rendezvous rather than chasing seek latency

A one-frame acceptance check against the playhead sampled before seeking can
refuse an otherwise usable successor whenever seek latency exceeds one frame.
Apple now parks one wall-second ahead, expressed in media time using the
viewer's rate, proves the parked decoded frame, and waits for the incumbent
to reach that film instant. The eight-millisecond observation loop ends at the
original overlap deadline or the existing alignment bound. Pause, background,
rate change, stale ownership or passing the frame window retains the incumbent.
There is no new overlap clock and no seek of the visible player.

The authored rendezvous regression covers the future lead, unchanged item
origin, negative lead and overflow. Inspection also corrected a fixture edit
that had accidentally changed a rejected 8.251-second sample into 8 + 1/241;
this was found during source review, before unit execution. The rejected
sample is now 8.05 seconds for a 24 fps boundary. iOS production/test-source
compilation passed; physical timing remains unmeasured.

Pinned Media3 1.10.1 source inspection identified a separate Android design
constraint: `HlsSampleStreamWrapper.selectTracks` resets buffered media when
the primary selection object changes. Replacing public track overrides alone
cannot prove future-load continuity. The native adapter needs one retained
selection object whose chosen index changes internally; queue retention and
physical disposal still need integration. The inspected implementation is
[the pinned upstream source](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer_hls/src/main/java/androidx/media3/exoplayer/hls/HlsSampleStreamWrapper.java).


A fresh isolated native Safari probe on October 2 also timed out creating its
WebDriver session (`Safari POST /session`, 15-second deadline). Its own driver
and local HTTP server were closed. The failed receipt remains
`safari-native-autonomous-blocked.json`; no native media or switch evidence
was obtained, and no lock-state diagnosis is inferred from that timeout.
The future-rendezvous iOS production/test-source and tvOS production builds
both passed. No unit test was executed.


### 10.88 Android preserves the metadata named by its first-render event

The renderer thread can submit several frame metadata callbacks before the
application looper receives its first-render event. The pending slot now keeps
the first valid frame until explicit seek/output invalidation, so later queued
metadata cannot replace the picture used by alignment. The authored regression
queues a later frame before render settlement and requires the original PTS,
then checks seek invalidation and a fresh frame. Android production and test
sources compile; unit execution remains deferred.

This concerns the ordinary renderer metadata path. Pinned Media3's tunneled
buffer callback does not invoke the standard frame-metadata listener, so TV
PTS proof still needs its renderer integration. It is not claimed from a
format change or the first-frame event alone. The inspected paths are in
[the pinned video renderer](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer/src/main/java/androidx/media3/exoplayer/video/MediaCodecVideoRenderer.java).
Physical-device and audible/display qualification remain outstanding.


### 10.89 Tunneled Android rendering supplies its actual frame timestamp

Finite and prepared players now use a narrow subclass of the pinned codec
video renderer. Its tunneled hardware callback resolves the decoded format
for that timestamp, subtracts the renderer's stream offset and sends the
item-local frame metadata before the usual first-render event is posted.
The ordinary metadata path stays with Media3. Unset, negative, end-of-stream
and overflowed timestamps cannot satisfy alignment. The authored regression
maps a large codec offset into the same one-frame boundary as ordinary output.

The factory preserves the inspected 1.10.1 defaults: codec adapter, decoder
selection/fallback, joining allowance, 50-frame dropped-frame reporting,
AV1 dependency parsing, late-input threshold, duration scheduling and TV
tunneling. Audio/text construction remains inherited. Explicit extension
renderer configuration keeps its existing factory path; the application
currently requests the bundled codec renderer. No new decoder or permanent
audio graph is added. Android production and test-source compilation passed.
This is implemented timestamp integration, not physical presentation proof.
No phone/TV appears in ADB or its mDNS inventory; device qualification remains.


### 10.90 Android rendezvous waits for decoded readiness and stays live after a miss

Seek completion and contiguous buffered media can precede the parked output's
first-render callback. Warm-output rendezvous readiness now requires that
actual output frame at the intended film instant. Once ready, the meeting
window is bounded by its decoded frame duration rather than the older
quarter-second scheduling allowance. The final check reads the incumbent
again. A refused final check leaves the bounded rendezvous coroutine running,
so it can observe or repark instead of leaving the preparation idle until its
overlap deadline. The original physical deadline still bounds every attempt.

Authored regressions distinguish an early observation, an accepted one-frame
arrival and a 100 ms miss at 24 fps, and reject invalid cadence. Android
production and test-source compilation passed (`compileDebugKotlin` and
`compileDebugUnitTestKotlin`, 13 seconds). No unit test was executed. This
repairs the prepared path; the continuous adapter and physical qualification
remain unfinished.


### 10.91 Android converts rendezvous seek latency into the incumbent's film clock

The measured seek duration is wall time. The old repark calculation added it
to a film-time lead without applying playback rate, so a fast incumbent could
outrun every attempt despite a usable decoder. The initial estimate and the
observed seek duration plus margin now become film distance at the current
rate. Paused or invalid rates use the ordinary parked estimate; calculation
saturates rather than wrapping near the timeline bound. The controller keeps
the same physical overlap deadline and bounded repark count.

The authored clock-model regressions cover 0.5x, 2x and 8x delayed seeks, a
fixed wall-time estimate across rates, saturation and bounded failure beyond
the supported lead calculation. Android production and final regression
sources compile. Unit execution remains deferred until the final review;
physical timing and the continuous adapter remain outstanding.


### 10.92 Android transaction foundation — compiled, not yet enrolled

The native adapter now has a version-one receipt validator, bounded serialized
exchange owner and profile-bound HTTP transport. The owner retains uncertain
requests across failed acknowledgements and coroutine cancellation, replays
identical wire data before newer intent, and bounds queued exchanges at 32.
Rate-limited retries wait at most one second. Only durable End proof permits
terminal reconciliation. Incoming identities, sequences, revisions, intervals,
transaction collections, shared-audio pins, total pin count and byte sum are
validated before advancing state. String booleans and unsafe integers cannot
stand in for protocol values. Family binding verifies the two-to-eight-rung
AVC clock and raster graph, playlist paths and optional shared AAC metadata.

The HTTP client captures the creating profile, checks the exact same-origin
schedule path, rejects redirects, bounds both response and request bytes, has
a fourteen-second call deadline, rejects malformed UTF-8 and cancels its owned
calls on close. Authored regressions cover lost acknowledgement ordering,
cancellation, delayed rate limits, foreign and malformed receipts, terminal
proof, family binding and cross-transaction reservation bounds. Compilation
of production and test sources is the evidence here; unit execution is
still deferred. These foundations do not enroll playback by themselves.
The retained track-selection, reserved data source, accepted-sample ownership
and disposal integration remain required before native enrollment is wired.
No new feature switch or readiness gate was introduced.

Pinned Media3 exposes the HLS extractor output and the actual public
`SampleQueue` indices. A metadata callback can be rejected by the queue;
therefore append proof needs the accepted write-index change, observed under
its monitor rather than an unsynchronized UI poll. Front indices establish
queue retirement, but cannot establish release of decoder or audio-sink
ownership. The adapter must combine real queue lifetime with actual renderer
and sink observations, and fence queue resets and reused extractors. These
are implementation constraints, not qualified native playback evidence.
The inspected implementation is
[SampleQueue 1.10.1](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer/src/main/java/androidx/media3/exoplayer/source/SampleQueue.java).


### 10.93 Android retains its selection object and publishes only reserved intent

The native selection component matches actual AVC track-group formats to the
frozen family and caches one selection object for unchanged supported indices
and type. Future-load intent is published from the owning protocol's accepted
latest transaction only when an undisposed reservation covers the requested
frontier. Preparing, cancelled, superseded, unreserved or conflicting intent
cannot replace the published choice. The selected index accounts for Media3's
bitrate ordering; source track indices are not assumed to be selection indices.

The component retains queued chunks, declines automatic load cancellation and
never excludes a track to select an unreserved substitute. Ordinary periods
use the existing adaptive factory. A controlled period without a verified,
reserved group refuses instead of falling through to an independent ABR
owner; enrollment must handle that refusal before adoption. Existing native
Auto remains the quality-policy owner. The component is not yet connected to
runtime enrollment: reserved loads, actual accepted-sample ownership, disposal
and optional-failure retention still need integration.

Authored regressions cover the real selection-index reorder, stable instance
reuse, reservation-before-selection, an interval boundary, stale/unknown
choices and supported-definition changes. Production and regression sources
compile. Unit execution and physical buffer-retention proof remain deferred.


### 10.94 Android verifies reserved media bytes before extraction

The native media verifier resolves resources under the captured same-origin
session parent, checks rendition and init paths against the immutable family,
and bounds every payload at 16 MiB. Initialization bytes must hash to their
exact descriptor identity. Fragment bytes must match an accepted, undisposed
reservation by hash, rendition, timescale and length. Video fragment numbers
also bind to the declared rational-grid interval, including a shorter tail;
frontier arithmetic refuses overflow. Every matching transaction owner is
retained for the future append/disposal observer rather than arbitrarily
crediting one when the same artifact has several owners.

The server already verifies the reserved fragment's actual fMP4 sample grid
before publishing that hash; matching the bytes preserves that provenance.
This verifier does not establish queue acceptance or rendered presentation.
The loader must still reserve before a cold request, fence its attachment,
reject foreign requests within its bound media source, and connect actual
sample/decoder/sink ownership. Authored component regressions cover init and
fragment byte changes, missing and disposed pins, foreign identities, segment
interval mismatch, payload bounds and overflowing frontiers. Production and
test sources compile; unit execution remains deferred. Native enrollment is
still not connected.


### 10.95 Android reads the actual AAC decode clock before reservation

A bounded structural reader extracts the first single-track fragment decode
clock, refusing duplicate, truncated, malformed and unsafe clock boxes. Exact
integer scaling maps that AAC clock to its containing video entry without
intermediate overflow. This chooses the reservation window; it does not
establish sample acceptance or presentation. Production and regression
sources compile. Authored clock and malformed-input regressions await the
final fast lane. Native loader and ownership integration remain unfinished.

The native UI tool confirmed on October 2 that the Mac is locked and could
not unlock it automatically. Safari qualification is pending a manual unlock.
The isolated nuc3 source transfer remains pending explicit destination
authorization following automatic approval rejection. Independent native
source work continues while those qualification paths are unavailable.


### 10.96 Android connects reserved loading to actual queue acceptance

The controlled data source uses a captured-profile upstream, confines every
request to the owning parent and family playlists, reserves cold video before
its GET, and reserves missing AAC pins using the actual decode clock. It
verifies the complete immutable artifact before exposing any retry slice;
inherited Range headers cannot turn that verification into a partial read.
Media payloads remain bounded at 16 MiB and VOD playlists at 2 MiB. Pending
reservation cancellation and final byte publication share a lifetime fence.
Loader-thread provenance remains available until close, including extractor
callbacks after a read returns. It carries the actual attachment owner.

The reservation owner serializes initial and changed demand, schedules exact
ready intervals before publishing the retained selection, and refuses an
unsupported target before preparing it. Actual supported formats are
published from the selection thread as an immutable snapshot. On uncertain
optional admission the previous demand is retained; pending ordering must be
replayed before a restorative preparation can enter the ledger. A failed
optional admission now attempts that restoration within the caller's remaining
budget; cancellation propagates to the owning teardown instead.

The extractor wrapper delegates the pinned Media3 implementation and checks
the queue write index before and after metadata insertion under its monitor.
Rejected metadata never credits acceptance. Reused extractors use the current
verified load context rather than the URI from extractor construction. A
bounded inventory requires extraction EOF, the expected accepted sample
count and a contiguous single-queue span before crediting an entire append.
Queue front removal remains distinct from decoder and audio-sink release.
No requested discard or player clock is treated as physical disposal.

Each constructed player now has its own registry. A wrapped controlled source
binds selection before creating the actual Media3 period and removes only
that binding on release. Ordinary periods retain the adaptive selection
factory. The controller still needs enrollment, quality intent, render/sink
ownership and teardown integration; these components do not qualify native
playback by themselves. Production and regression sources compile, including
reservation ordering, unsupported target retention, retry ranges and actual
supported-format snapshots. Unit execution remains deferred to the final
fast lane after adversarial review.


### 10.97 Android binds bootstrap and output facts to their creating owner

The native bootstrap negotiates bounded candidate pairs on a captured profile,
validates candidate digests and the controlled family, and requires the exact
parent schedule, descriptor and master-playlist paths. A malformed admitted
bootstrap attempts bounded parent release on that same profile. Unsupported
codec, grade, burn or native-text selections retain ordinary creation. Stable
request identities retain their family generation for negotiation retries.
The controller still needs to adopt the result and own its lifetime.

One captured-profile JSON transport now serves bootstrap, family and schedule
work. It refuses redirects, path normalization outside the requested API
path, malformed UTF-8 and oversized bodies. The authored local HTTP fixture
checks authority after another profile changes, redirect refusal, response
bounds and closed transport behavior; its sources compile without execution.

The output observer captures each processed video frame's format and owner
against its actual codec timestamp, including the offset for a skipped flush.
A hardware frame callback consumes that exact record; later format changes,
reset epochs and another attachment cannot borrow it. It preserves Media3's
existing tunneled callback and prepared-frame metadata. The bounded timestamp
map refuses unsafe positions and cannot grow beyond 512 entries. Audio-head
observations come from the actual sink, normalized at its stream offset.
A returned sink flush/reset is recorded separately and does not establish
complete audio ownership release or externally audible continuity.

Successful codec flush and release primitives report decoder disposal. The
renderer state-reset hook is insufficient: Media3 calls it from a finally
block even when flushing throws. The wrapper therefore credits no decoder
release on an exception. Authored regressions cover this failure, timestamp
format binding, older epochs and attachment ownership. Production and test
sources compile. Actual AudioTrack retirement, controller adoption, optional
quality changes and complete terminal reconciliation remain to be connected
and physically qualified. No unit tests or final adversarial review ran.
The inspected implementation is
[MediaCodecRenderer 1.10.1](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer/src/main/java/androidx/media3/exoplayer/mediacodec/MediaCodecRenderer.java)
and [MediaCodecVideoRenderer 1.10.1](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer/src/main/java/androidx/media3/exoplayer/video/MediaCodecVideoRenderer.java).


### 10.98 Android bounds future loading and observes actual audio retirement

Controlled periods retain Media3's existing allocator and byte budget while
limiting new loading to eight seconds of playout at the current speed. Ordinary
periods retain their original load control. This bounds future rendition-change
latency without removing buffered media. The authored regression covers period
ownership, playback-rate conversion and the allocator refusing further load.

The audio provider preserves the pinned SDK's default track construction and
captures its actual AudioTrack. Retirement requires STATE_UNINITIALIZED for
every track belonging to that attachment. Media3's release notification is
insufficient: its finally block can notify after a failed flush, and its posted
callback can be lost after the playback looper stops. The attachment can poll
the bounded ownership inventory independently. Authored regressions cover
failed or unknown release state, multiple outputs and capacity recovery.

A cold video load selected just before a reserved quality change now has a
distinct pre-fetch refusal. The controlled-source error policy can request
track fallback for that zero-byte failure. The retained selection accepts it
only when a supported, valid reserved choice exists and differs from the
failed track; it installs no blacklist that could obstruct a later explicit
choice. The pinned HLS loader removes this failed chunk and keeps the previous
queued chunks. The regression also rejects stale, conflicting and unsupported
choices and verifies returning to the previous rendition.

Attachment cancellation now closes admission and cancels owned I/O separately
from loader retirement. The extraction lease survives cancellation until the
loader's own close completes. Queue inventory keys include rendition and
artifact, can observe actual empty queues after writers retire, and retains
partial extractions until proven disposal. A media-period release callback is
explicitly a release request, because HLS frees queues asynchronously.
Production and test sources compile in 13 seconds; no unit tests ran.
Controller adoption and terminal reconciliation remain unfinished. Relevant
pinned implementations are
[HlsSampleStreamWrapper 1.10.1](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer_hls/src/main/java/androidx/media3/exoplayer/hls/HlsSampleStreamWrapper.java)
and [AudioTrackAudioOutput 1.10.1](https://github.com/androidx/media/blob/1.10.1/libraries/exoplayer/src/main/java/androidx/media3/exoplayer/audio/AudioTrackAudioOutput.java).


### 10.99 Android adopts one retained continuous source and its terminal owner

Compatible initial finite HLS creation now negotiates a continuous parent on
its captured profile, reserves its initial load window and installs the bound
HLS source. The controller retains that source for compatible manual and Auto
quality requests. Those requests publish the existing viewer intent, reserve
at the current buffered film frontier and update the retained selection;
they do not wait for a legacy prepared offer or replace the media item.
Actual hardware presentation, after accepted queue samples and durable append
receipts, settles the displayed candidate and quality recipe. It does not
certify a concurrently changed audio or subtitle media recipe. A presented
quality updates the retained plan's authority so later seeks do not reopen
merely because the original plan named the previous quality.

The attachment owns bounded append observations, exact transaction ordering,
frame-grid conversion and disposal. It can credit a newer exact reservation
against samples physically accepted and still retained in this attachment,
without inventing another append. Actual queue retirement plus rendered video
or audio sink progress authorizes normal disposal. End closes admission,
cancels only captured media calls, joins its fact pump, ends the captured
parent and reconciles the terminal ledger. It reports disposal only after
loaders, queues, decoders and AudioTracks have actually retired. Its cleanup
scope survives composition cancellation and has one five-second bound;
missing proof remains a failure, never an empty-ownership receipt.

Media3's ordinary data-source close does not cancel a call still waiting for
headers. The attachment therefore wraps its captured Call.Factory, retaining
calls through body close and cancelling them explicitly. Control exchanges
have a separate owner and remain available for End. The authored regression
covers body lifetime, pending-header cancellation, refusal of later media
calls and leaving independent control calls untouched.

The controlled future-load limit is now twelve seconds of playout. Inspection
of the existing Auto policy found that its upgrade branch requires ten
seconds of buffered media, so the earlier eight-second bound could prevent
upgrades. The allocator's byte budget still wins and existing buffers stay
retained. Authored clock regressions cover 24 fps microsecond rounding,
30000/1001 cadence, segment frontiers and overflow. Production and test sources
compile in 14 seconds. No unit tests or adversarial agent review ran.

Android source integration still needs pending-quality seek/race coverage,
optional presentation timeout handling, restart and teardown qualification,
and physical phone/TV audio and display evidence. The existing main-backed
Chrome/Firefox production replay and Apple/Safari qualification also remain.
The native UI tool was checked again on October 2 and still reports the Mac
locked. Source transfer to the isolated runtime remains pending the explicit
approval requested after automatic approval review rejected the transfer.


### 10.100 Android keeps pending-quality seeks and discarded queue epochs separate

A compatible quality request no longer changes the controller's executed
video recipe before hardware presentation. A seek during that request stays
on the retained source and executes against the quality actually presented;
the desired quality remains a separate viewer preference. After the owned
frame receipt, the controller advances the recipe and retained plan. Audio,
subtitle, Original and out-of-family media changes retain their replacement
semantics.

The queue inventory now observes actual reset generations. Reading or front
removal preserves absolute sample indices and cannot establish reset. A zero
write index after accepted samples, or an observed empty queue with all three
indices zero, retires the previous generation. Fresh acceptance of the same
artifact starts a new physical span rather than borrowing earlier append
counts. Partial extractions can retire after the independently observed
reset and actual decoder release.

AAC disposal across a seek additionally requires a successful sink flush and
actual retirement of every AudioTrack allocated before that flush. A new
active AudioTrack cannot hide that retirement, and cannot be credited as
released itself. Codec wrappers retain their creating attachment even when
a successful empty flush happens before the first input; an older wrapper
cannot credit another subscription. Normal front disposal still requires
actual rendered video or advancing sink head past the artifact.

Authored regressions distinguish reset from normal front consumption, isolate
queue generations, reject invalid sample indices, separate old and new audio
outputs, and retain an empty codec's creating owner. Production and test
sources compile in 11 seconds. Unit execution remains deferred to the final
main-ready adversarial review and fast lane. Physical seek/disposal evidence,
optional observation deadlines and restart races remain unqualified.


### 10.101 Android preserves committed observation uncertainty

The existing playback sampler now supplies the continuous attachment's clock,
rate and active state. Only committed target intervals with actual accepted
queue samples can start an observation deadline. A future buffered boundary
cannot consume the grace period. Once the observed clock reaches that
boundary, two seconds of active wall time without the target hardware frame
reports observation unknown once for that intent. Pause and background time
are excluded; rate changes and an independent seek update the boundary clock.

That outcome keeps the committed media and pending preference, continues
observing late hardware frames and does not silently restage, reopen or claim
retained current. The notice cannot repeat for every later artifact in the
same unresolved intent. Quality settlement requires an actual numeric
presented tick in the durable receipt; an omitted optional field is not
presentation proof.

Authored regressions cover a forty-second future boundary, a long pause,
changed playback rates, seek clock reset, new revisions and one notice per
intent. Production and test sources compile. Unit execution and formal agent
review remain deferred to the final main-ready candidate. Optional target
load failure before exposure, control-phase cancellation and their runtime
race/pressure evidence remain to be completed.


### 10.102 Android serializes physical retirement against artifact reentry

Disposal now establishes a per-resource read barrier atomically with loader
admission, after all current loaders have actually closed. The queue retirement
proof is rechecked inside that admission guard. A newly opened loader waits
before reservation or extraction of the retiring resource; unrelated resources
remain available. The barrier survives an uncertain disposition and is released
only after its exact protocol work settles and the old provenance is removed.
Cancelled readers do not remove another reader's barrier.

An AAC hash missing from the shared reservation now renews its window even
when the video ready window already covers the same entry. Shared audio can
be reserved again under a transaction whose historical disposed list contains
that hash, so audio retirement and terminal cleanup no longer skip the new
physical reservation on that historical membership alone.

Production and test sources compile in eleven seconds. A regression exercises
blocked reentry, cancellation, independent resources and barrier reuse; unit
execution remains deferred to the final main-ready fast lane. Optional target
load failure before exposure and native runtime qualification remain open.


### 10.103 Android retains a failed optional target only before exposure

Video byte publication and unexposed cancellation share one ownership monitor.
A target is eligible for restoration only when the previous rendition has a
durable hardware presentation receipt, the target has never exposed bytes,
and no accepted physical queue span aliases its reservation. Appended or
partially extracted media stays on the committed observation path.

A failed cold target load cancels its unappended transaction, restores the
previous rendition with a higher intent and accepted reservation, then yields
the zero-byte failed chunk through the existing retained selection. Manual
failures retain the viewer request for retry or explicit restart. Auto failures
settle only their matching private request token. A late failure cannot settle
a newer manual request or Auto candidate. Canceled reservation cleanup waits
for actual loader closure and uses the same disposal/read barrier. Exposure
fences are pruned only while loaders are quiescent.

Authored regressions cover exposure before cancellation, cancellation before
publication, physical alias refusal, mixed-owner publication refusal and
restorative control ordering. Production and test sources compile; unit
execution remains deferred to the final main-ready fast lane.

The native UI returned Safari's accessible page once on October 2, proving
the Mac had become unlocked at that moment. A renewed WebDriver probe still
timed out creating a session. The next native action reported a locked Mac
again. No media evidence was obtained; the isolated fixture server was closed.
Production replay, physical native qualification and source-transfer approval
remain outstanding.


### 10.104 Android requires operation-specific receipt evidence

Receipt validation now checks the requested operation's durable result in
addition to identity and bounds. Scheduled intervals must be reserved;
appended intervals must be appended; presentation requires a durable first
tick and the requested artifact covering its observed tick. Cancellation must
be acknowledged with completed intervals preserved. Disposal must list the
artifact as disposed and remove its ready, reserved and appended entries.
Malformed acknowledgments remain pending and cannot advance the ledger.

The new regression supplies correctly identified but unperformed operation
receipts and verifies rejection, then supplies actual operation facts. Source
inspection also corrected the restoration fixture's simulated presentation to
include its accepted append, observed timestamp and ever-appended flag; a
first tick alone is not a valid presentation receipt. Production and test
sources compile in twelve seconds. No unit tests were executed.


### 10.105 Android separates End from physical cleanup and bounds control work

Ending the active server session now closes admission and sends captured-profile
End immediately, without unsubscribing output observers or starting the physical
cleanup clock while the controller is still constructing a replacement. Actual
player stop, source removal or player release starts the bounded reconciliation
phase. That phase awaits the durable End and then observes loader, queue, codec
and actual AudioTrack retirement before sending disposal. Unknown retirement
remains unacknowledged. Unadopted sources still finish directly because no player
ever acquired them.

Unappended scheduling and revalidation now have a thirty-second active-time
budget per intent. Pause and background do not consume it. Expiry attempts the
same proven-unexposed restoration path; any byte exposure or physical alias
refuses retained-current, and committed target media stays on the independent
observation deadline. Authored regression source covers pause, independent
revisions, one expiry per intent and committed-target exclusion. Production and
test sources compile in twelve seconds. No unit tests were executed.

The remote main has advanced from the prior integrated c63859963 to 4f55ae17e
with native startup/readiness and producer-capacity fixes. Completed CQ commits
are backed up through a73f0add9; this newer main needs integration and matching
compiler evidence before runtime qualification or final review.


### 10.106 Current main integration preserves continuous publication ownership

Main 4f55ae17e is integrated into the batch. The single playlist conflict was
resolved by preserving continuous-family master publication before the ordinary
master path, while retaining main's readiness deadline for ordinary session and
context lookup. Each publication budget starts after the corresponding ready
resource is available. Native readiness and producer-capacity fixes remain
intact. Web player, startup/failure ownership and documentation changes merged
without textual conflicts.

Rust 1.97.1 (8bab26f4f) workspace/all-target source and test-source compilation
passes against this integrated tree in 59.21 seconds. The normal commit hook
provides formatting, all-target Clippy and served-JavaScript syntax evidence.
Unit execution, adversarial review, exact-source production replay and physical
native qualification remain pending. No runtime receipt from the older Linux
binary qualifies this merged source.


### 10.107 Caller cancellation cannot reopen an optional continuous change

Continuous manual control failure no longer enters the generic prepared-change
reopen path when the incumbent is buffering or its health is unknown. An
unsupported target retains immediately because it issued no target prepare. An
uncertain control result preserves the pending request, offers Retry or explicit
restart, and leaves genuine playback failure to the existing recovery owner.
Auto uncertainty retains its pending candidate rather than restaging every tick.
Late notices and settlement require the matching request and media epoch.

The attachment scope owns restorative work after caller cancellation. It
replays the exact pending request, refreshes the ledger, checks zero exposure
and absence of any queued target-rendition alias, then cancels eligible target
work and reserves the previous choice through a higher intent. Only a durably
presented incumbent and proven-unexposed target produce retained-current. An
uncertain or committed target cannot borrow that label. Recovery survives the
caller's twelve-second timeout and remains bounded by owned transport requests;
End still closes the attachment's scope and captured transport.

The reservation regression now cancels an in-flight target prepare, verifies
exact replay before cancellation and restoration, and checks the original
request token. Production and test sources compile in thirteen seconds. Unit
execution remains deferred to the final main-ready fast lane. Read-only ADB
inventory still has no physical devices. Runtime ordering, pressure, display,
audio, and terminal cleanup qualification remain unmeasured on this exact tree.


### 10.108 Mobile release counters and compilation match integrated main

Android versionCode is 143, above main's 142. Apple CURRENT_PROJECT_VERSION
is 205, above main's 204, shared by iOS and tvOS. Semantic version remains
0.3.0. The ignored Xcode project was regenerated from the tracked project.yml
in the independent clone. Android production/test sources compile in nineteen
seconds; iOS build-for-testing and tvOS production build both succeed. These
commands compile test sources but execute no unit tests and install no apps.

The merge-target counter validator reads committed HEAD in changed-from mode,
so its precommit invocation correctly still saw the old counters. It is checked
again after this commit against origin/main. Physical app and output evidence
remain separate from compilation. The local ADB inventory is empty and the
read-only node inventory found no existing ADB service; no remote ADB service
was started and no source was transferred.


### 10.109 Coalesced actor wakeups preserve actual active-time budgets

The controller records every transport and lifecycle sample before actor wakeups
coalesce. Its accumulated active clock excludes a pause/background interval
even when a network exchange prevents the actor from observing both transitions.
Deadline consumers distinguish that accumulated metric from ordinary wall time,
so a previously observed pause cannot discard later active time either. Film
position and playback rate remain observed values; an invalid rate cannot
produce a speculative boundary crossing.

Restorative prepare acknowledgment can also arrive only on exact replay. The
recovery owner now reuses that live acknowledged transaction instead of issuing
another intent on every retry or after the incumbent loader restored it. The
regression loses two restorative acknowledgments and verifies the revision does
not grow on replay. Clock source regression covers a long background interval,
coalesced resume, active control and presentation budgets, and invalid rate.

Production and test sources compile in eleven seconds. One earlier invocation
pointed ANDROID_HOME at the Gradle cache, failed before compilation and was
corrected to the existing SDK; no SDK package or license directory was created
in that cache. Unit execution remains deferred to final review and fast lane.
Committed mobile counter validation passed against origin/main before these
source-only changes. The branch is backed up through 0515f1d2a.


### 10.110 Android disposal waits for actual compressed-allocation release

The retained load-control wrapper now observes allocation and successful release
through Media3's public Allocator interface while delegating the original byte
budget and allocator accounting. A write conservatively associates all live
allocations of this attachment with its artifact. Shared allocations may delay
disposal; another attachment's allocation cannot prove retirement. Failed
single or batch release leaves ownership unknown. The inventory is bounded to
4096 allocations and 128 artifacts per allocation; the existing player byte
target remains at most 64 MiB and is unchanged.

Verified byte exposure now creates provenance before extraction. Zero accepted
samples do not invent an append or queue-front proof. Actual metadata acceptance
from other output formats is kept as physical provenance, while only verified
video/AAC samples count toward a complete expected append. An artifact with no
accepted samples can retire only after loader closure and release of every
associated allocation. Normal front/reset disposal additionally waits for those
allocations, and terminal disposal requires all owned allocations released.

A second read barrier is keyed by the verified artifact digest after fetch and
before reservation/authorization. It protects AAC aliases under different media
URLs as well as same-URI reentry. Both resource and digest barriers release
only after exact disposal acknowledgment and old provenance removal. The two
barriers per artifact are bounded to 256 entries for 128 artifacts.

Authored regressions cover shared allocations, separate owners, reused allocation
identity, failed single and batch release, unchanged byte accounting, exposed
bytes with no accepted metadata, and same-digest URL aliases. Production and
test sources compile in thirteen seconds. No unit tests were executed. Physical
allocator/codec/audio cleanup and runtime pressure qualification remain pending.
The Android compiler invocation now uses a temporary script with fixed existing
SDK and Gradle-cache paths; that script belongs to final task cleanup.


### 10.111 Android records the expected committed boundary delay

The attachment records target boundary PTS and expected active delay before
acknowledging an accepted append, using current film position and playback
rate. Later boundary, rate or active-state changes refresh that evidence. A
paused or invalid clock reports an unknown delay rather than promising a wall
time. Healthy retained prebuffer can legitimately delay presentation beyond
thirty seconds; it is informational telemetry, not a degraded-playback warning.
The observation grace still starts only when the actual clock crosses the
committed boundary. The source regression covers slow and fast playback, pause,
invalid rate and overflow; production and test sources compile without unit
execution.

The human explicitly approved committed-source transfer to
pjunod@192.168.4.7 under /tmp/plurx-cq-cd9bb38ad/, without .git or credentials.
That resolves the earlier destination-authorization rejection. The next Linux
qualification build uses this committed tree; older runtime receipts remain
failed or unqualified and are not reused as current evidence.


### 10.112 Current main catalog integration preserves both continuous recipes

Main advanced to 15e36f7f4 with canonical catalog, scoped worker revalidation,
startup allowance and planning-generation changes. The batch now carries those
changes. Published replicated planning schema v69 precedes quality cancellation
v70 and dependencies v71; published SQLite planning v91 precedes cancellation
v92 and dependencies v93. Earlier ledger migration numbers describe their
then-current unpublished base and are superseded by this sequence.

Continuous starts reuse the canonical catalog and startup allowance. The exact
companion recipe is carried inside the negotiated continuous-media envelope and
revalidated alongside the primary against one retained source/settings snapshot
and decoder capability set. Each validation scopes to its selected recipe; the
worker does not enumerate another complete catalog. The local companion context
remains omitted from wire. Incomplete or non-dispatchable workers are not
advertised as a continuous family. Authored source assertions cover those rows
and the companion descriptor's identity, strict parsing and wire retention.

Rust 1.97.1 workspace and all test-source targets compile. The compile loop
identified context boxing, retained-snapshot API, fixture and derive mismatches;
these were repaired locally before commit. No unit tests ran. Android production
and test sources compile at versionCode 144; iOS build-for-testing and tvOS
production build pass at Apple build 206, above current main's 143 and 205.

The approved Linux build of 9565c98c1 succeeded in 121 seconds. It predates this
main integration and is not qualification evidence for the integrated branch.
The next source archive refreshes that isolated build before playback probes.
Native Safari's UI action again reported a locked Mac; its local fixture server
was closed. Physical Android, Apple display and audio evidence remain pending.


### 10.113 Exact current-code startup refusal exposed derived-role evidence

The full twenty-switch Chrome case started against Linux build 649687dc3,
verified by its system response. It failed before the first frame: zero VOD
session creates and continuous-role incompatibility. The failed receipt is
retained as continuous-chrome-full-linux-649687dc3.json; it is not a partial
pass. The launcher closed its isolated daemon and removed its browser runtime.

A standalone companion-video or shared-AAC request cleared its parent companion
identity and local context but retained the newly serialized companion catalog.
Both recipe derivations and the later VOD AAC attachment now clear that
parent-only evidence together. AAC also drops video-only candidate context
and the parent family descriptor. The existing
real worker-role regression also constructs a canonical two-video/AAC family
and verifies it reaches the deliberately denied capacity result, preserving
the incumbent, rather than failing recipe validation. Its source is compiled
by the normal hook; unit execution remains deferred to final review. The next
probe starts a fresh fully identified series after the source rebuild.


### 10.114 Current-code playback exposed inherited-wait and physical-pin bounds

Linux build fea65d8fa compiled in 121 seconds. The fresh twenty-switch Chrome
series created one VOD parent, started its first frame and recorded a 720p
target presentation at film tick 240. Its next 1080p request retained current
and the series failed. Repeated later transitions reached Capacity. The failed
receipt remains continuous-chrome-full-linux-fea65d8fa.json; a target
presentation alone is not continuity or physical display/audio qualification.

Private preparation inherited up to twelve seconds but each child wait was
capped by the ordinary eight-second media-request budget. Preparation now
spends only its inherited remaining allowance across both video and AAC;
ordinary HTTP media retains the configured cap. A bounded reason records
Pending, deadline, capacity, source or owner/media refusal without raw errors.

Physical dependency accounting now counts an exact immutable interval once
across logical intent owners. Conflicting interval facts for one rendition/hash
are rejected. The 128 dependency and 256 MiB physical limits remain unchanged.
Each intent still retains its own reservation and append/disposal facts. Replay
records retain a SHA-256 digest of the entire canonical transition request
instead of duplicating operation interval arrays. Exact replay restores the
original acknowledgment; a changed payload is a conflicting replay. The count,
ninety-second horizon and 128 KiB serialized ledger bounds remain unchanged.

Authored regression covers two logical owners of 64 video plus 64 AAC objects,
one physical budget, persistence and exact lost-ACK replay, altered payload
rejection, conflicting immutable facts and a genuinely excessive new dependency.
Source compilation and normal commit checks precede the next Linux rebuild;
unit execution remains deferred to final main-ready review and fast lane.

The production harness now ends a continuity series immediately on an explicit
retained-current result instead of waiting another ninety seconds for a frame
that the failed optional request cannot produce. This remains a failed series;
healthy playback continuing does not qualify the requested change. JavaScript
syntax is checked without executing the unit suite.


### 10.115 Current-code refusal and native Safari measurement

Exact Linux build bc572ca2d passed compilation in 283 seconds. Its fresh full
Chrome series again presented 720p but retained current on the next 1080p
request. The failed receipt remains continuous-chrome-full-linux-bc572ca2d.json.
The isolated target cache stopped at segment 30 while preparation requested
segment 36. Admission's boundary could be overwritten by an ordinary control
heartbeat before the private segment wait registered; speculative production
then started behind the requested future boundary. A separate preparation
frontier now drives optional ahead-fill while ordinary control retains its
playback anchor. Settlement clears the preparation frontier on scheduling,
cancellation or refusal. Dependency pins and ordinary playback windows retain
their existing authority. Pending diagnostics recognize the actual structured
VodError variant. The authored regression covers a heartbeat arriving between
boundary admission and wait registration; test execution remains deferred.

The Mac accepted native Safari actions after unlocking. A fresh isolated
synthetic master recorded 426 frame callbacks through 20.01 seconds at
1920 by 1080, one selected VideoTrack, and no exact-height control API.
The receipt is safari-ui.json under the ignored continuous-quality evidence
folder, with fixture and probe digests. This measures autonomous native fixture
playback only: no requested 720p switch, production transaction enrollment,
physical display capture or audio output is qualified. The experiment's tab
and local server were closed; production Safari pages were not changed.


### 10.116 Auto settles on durable target presentation

The frontier-repair Chrome series prepared and appended its second target,
then durably presented 1080p at film tick 1632. Its full series is still
running; this intermediate observation is not a qualification pass.

Web Auto previously released its move claim when continuous scheduling
returned, before the buffered target reached presentation. That path also
missed Auto's completion record and cooldown update. A continuous directed
change now retains its owner until its exact latest transaction has a durable
Presented receipt and the target candidate has an actual frame observation.
The existing settlement reducer records one Auto move and cooldown, then
releases ownership. A superseded, canceled or different same-rung transaction
cannot settle it. Continuous settlement clears the temporary requested height;
prepared handoff retains its existing acknowledgment rule. No ten-second
prepared timeout is attached to a healthy future buffered boundary.

The regression source covers scheduled-only, frame-without-receipt,
superseded, wrong-transaction, exact presentation and duplicate callbacks.
JavaScript syntax and normal commit checks run before committed-source
qualification; no unit execution is performed before final adversarial review.


### 10.117 Auto source coverage preserves the real policy

The AVC manual fixture also offers a compatible Original route to the normal
Auto catalog. That choice is outside the normalized two-rung family, so
forcing five continuous Auto changes by hiding Original would change the
policy being qualified. A separate thirty-minute 1080p MPEG-4 Part 2/AAC
fixture forces video encoding through ordinary capability negotiation. Its
catalog can exercise real Auto decisions between normalized encoded rungs
without removing candidates or replacing the policy. It has a deterministic
24 fps grid, two-second keyframes and a continuous 48 kHz sine soundtrack;
source generation is bounded to one encoder thread. The existing manual AVC
fixture and its qualification criteria remain unchanged. The new fixture is
source coverage; its switches and physical output are not yet qualified.


### 10.118 External review handoff before fast lane

The human updated the landing sequence on October 2: finish implementation
and qualification, obtain the adversarial agent review, then stop for external
Fable reviews. The handoff must include the concrete changes, agent findings
and unresolved issues. No fast-lane unit execution or main merge happens
before the human resumes work following that external review. This supersedes
the earlier autonomous review-to-test-to-merge instruction at that boundary;
independent implementation and current runtime qualification continue now.

### 10.119 First complete production Chrome manual series

The fresh isolated Linux Chrome run on exact committed source `6ad6c51c9`
completed twenty alternating 720p / 1080p manual changes in one playback,
with durable target presentation for every change. The machine receipt is
`target/playback-lab/reports/continuous-chrome-full-linux-6ad6c51c9.json`.
Its browser-video score passed: one durable VOD session creation, zero wait
events, reopens, stalls, hitches or dropped frames, with maximum and p95
frame gap 66.8 ms. Playback ran 1,193,142 ms including startup and observation.
This receipt qualifies that manual browser series only. It does not prove
physical display or audible continuity, real Auto decisions, Firefox, native
adapters, cancellation, seek, pressure or terminal producer retirement.

The later Auto settlement change is absent from that source snapshot.
Committed source `a7db2ac28` is now rebuilding with pinned Rust 1.97.1 in
the explicitly approved isolated Linux directory. The generated thirty-minute
MPEG-4/AAC fixture has been transferred there for real Auto qualification.
No unit test has executed, and the adversarial / Fable stop boundary remains
as recorded in §10.118.

### 10.120 Firefox launch diagnosis and retained-credit preparation frontier

The macOS Firefox 157 / geckodriver 0.37.1 run on `a7db2ac28` failed
before playback. A fresh explicit profile reproduced `Could not find
profile folder.` both with and without WebDriver. Mozilla records the
matching direct-exec failure on macOS 27 in
[bug 2062988](https://bugzilla.mozilla.org/show_bug.cgi?id=2062988).
No user profile or browser security setting was changed.

An official Linux Firefox 157 archive, verified against Mozilla's SHA256SUMS,
and geckodriver 0.37.1 were prepared under the approved isolated directory.
That browser started and entered the production quality cycle on `a7db2ac28`,
but the return to 1080p failed with `media_wait_pending` at frontier 2592.
The complete failed receipt and daemon log were copied to the independent
clone's ignored reports directory. This is failed production evidence,
not a successful Firefox series.

Inspection found that controlled admission returned immediately whenever the
target already retained a credit. That bypassed the preparation-frontier
update, allowing ordinary playback demand to remain its production anchor.
Admission now checks the attachment and source under the original lifecycle
and build gates, reuses retained capacity, and publishes the new preparation
frontier in both the warm and cold cases. A focused authored regression
`admitted_controlled_target_moves_preparation_frontier_without_duplicate_credit`
calls the actual admission path twice with an existing credit, checks the
production demands across an ordinary heartbeat, and proves the credit count
stays one until parent removal. This patch is not yet runtime-qualified.
No unit tests have run.

### 10.121 Cached init verification does not restart idle work

Pinned local compilation and the normal hook passed on `03e45c409`; the
isolated Linux rebuild passed in three minutes. Its fresh Linux Firefox
series still retained current on the return to 1080p (`media_wait_pending`,
frontier 2448), so the retained-credit repair alone did not resolve the
observed failure. The failed receipt and daemon log remain in ignored reports.

Inspection also found that `serve_init` armed materialization and kicked
the producer before checking for an already-cached immutable init. Family
verification calls this path for every rung. An idle rung could therefore
be woken at ordinary playback demand before target preparation installed its
frontier. The init path now registers its notification before the disk check
as before, but creates materialization ownership and wakes work only when
the init is actually absent. Source fences and the bounded missing-init wait
remain in place. Authored regression
`cached_init_read_does_not_wake_an_idle_controlled_producer` verifies an
actual cached read leaves no materialization owner or producer wake.
Pinned all-target compilation passed; no unit test executed. The change
and the repeated Firefox failure still require runtime qualification.


### 10.122 Interrupted encoded-source qualification

The exact `655b4e884` Chrome MPEG-4/AAC run entered the production manual
cycle and recorded nine completed target presentations before its log
WriteStream failed with `ENOSPC`. Revision 11 was appended but had no first
presentation in the last observation. The process exited without a final
case verdict and never entered the real Auto phase. This is an interrupted
run, not a successful twenty-change qualification. Its last ledger snapshot,
compressed log tail and producer census were preserved in ignored reports.

The census began after startup and sampled 277 times at approximately two
seconds. It saw one video plus one shared audio producer in 273 samples and
two video plus one audio in four samples. Sampling does not prove shorter
handoff overlap, initial admission or five-second terminal retirement. The
owned daemon was terminated after the probe exited; that forced teardown is
not application End evidence.

The failed probe's daemon logging is now a bounded two-MiB memory tail rather
than an unbounded live disk stream. The receipt will declare how many log
bytes were omitted. A fresh exact-source run has started; all qualification
rows remain open. No unit tests, final adversarial review or merge occurred.
The human will be notified when the final adversarial review is finished and
the handoff is ready for Fable, as requested in §10.118.


### 10.123 Startup isolation and native autonomous M0 evidence

The bounded-log Chrome retry on `655b4e884` failed before its first frame:
its local worker exceeded the placement deadline while startup FFmpeg
validation and caption probes were still active. The failed receipt is
`continuous-chrome-mixed-auto-linux-655b4e884-index-fixed-bounded.json`.
No isolated worker or FFmpeg process remained after its normal cleanup.
This startup failure is preserved separately from switching evidence.

A subsequent wrapper observed this daemon's own startup FFmpeg children
quiet for eight seconds before opening playback (49,014 ms observed wait).
It changes no product deadline, admission or quality policy. That exact-source
run is now progressing through the fifteen manual changes preceding real
Auto decisions; a fresh producer census began before playback. It has no
final verdict yet.

A separate generated native Safari M0 fixture exposes two real AVC High
Level 4.1 / 24 fps video playlists and one shared AAC soundtrack. FFprobe
verified 1280×720 and 1920×1080 against actual init plus fragment bytes;
manifest bandwidth comes from actual generated segment sizes. Its first
four-minute run observed native callbacks at 720p initially and 1080p from
film time 0.5 seconds, on the same video element with one video and one audio
track. It recorded 5,759 callbacks and maximum callback gap 83 ms. The
receipt is `native-safari-autonomous-short-pressure.json` in ignored reports.

The actual shaped-link interval did not produce a downgrade: Safari retained
1080p with advancing playback. Network `stalled` events were recorded and
must not be erased or called zero stalls. This fixture proves an autonomous
initial selection and upgrade only; production adapter switching, exact
manual selection, physical display, audible continuity and resource admission
remain unqualified. The generated fixture is not a production-family receipt.

The experiment tab was closed and its own server stopped. The Mac then locked
before a fresh longer-pressure experiment could start; manual unlock was
requested while independent browser work continues. No production tab,
profile or security preference was changed. The final adversarial / Fable
boundary remains pending, with no unit-test execution or merge.


### 10.124 Bounded removal evidence across real Auto observation windows

The startup-settled `655b4e884` Chrome receipt completed fifteen manual
changes and three real Auto presentations (1080p, 720p, 1080p), then failed
its evidence check before continuing to the fourth Auto decision. The third
Auto window contained 69 completed video removals and 69 audio removals,
while the browser observer intentionally retains only the last 64 per buffer.
Five earlier removals in that window were therefore absent from the final
snapshot. The complete case stays failed: no evidence gap is treated as a
pass, despite zero measured waits, reopens, stalls or hitches in that window
and a 66.7 ms maximum callback gap.

The qualification harness now folds each sampled removal interval into two
bounded counters, tied to the original SourceBuffer identities and starting
sequences. It rejects missing or non-contiguous observations, replaced
buffers, and removal ahead of the playhead. The final check binds those
counters to both original and final snapshots. The browser's 64-row journal
is unchanged; no unbounded removal history is retained. Existing callers
without a sampled proof still fail on a truncated final snapshot.

Authored regression `sampled removal evidence survives a long Auto window
and refuses observation gaps` drives the actual observer through 300 completed
removals, checks the journal remains bounded, and covers missed sampling,
future removal and replacement. Syntax checks passed; no unit test executed.
The mixed qualification will use this sampled proof on its fresh retry.


### 10.125 Admitted preparation demand precedes stale materialization waits

The fresh `f67d99aa4` Chrome run retained 720p on the return to 1080p and
failed before reaching its Auto phase. Its target frontier was 1152
(film time 48 s / plan index 24), while the target producer restarted at
index 7. This is a real preparation failure, separate from the sampled
removal-evidence repair. The failed receipt and bounded daemon log remain
in ignored reports.

The scheduler ranked existing blocked GETs at the reader's ordinary playback
anchor before its idle preparation hint. Waking an admitted target before
its later segment waiter registered could therefore restart at an older
request; registering the new waiter afterwards still ranked it behind that
old anchor. Cached-init and retained-credit repairs alone did not cover
this interval.

An admitted, bounded Prepare now publishes its actual waiting demand before
the producer wakes. Blocked requests belonging to that reader are ranked
against the preparation anchor while it is active; their old obligations
remain retained. Ordinary playback reporting and eviction windows are
unchanged. Preparation retains speculative capacity and does not overtake
another viewer's ordered foreground GET. Settling preparation returns the
reader to ordinary demand.

Authored regression
`admitted_preparation_outranks_its_stale_get_before_target_wait_registration`
checks the actual wait pool and production decision before a target segment
GET exists, another viewer's priority, and return to the original request
after preparation settles. The earlier heartbeat regression now expects an
active preparation wait rather than an idle hint. Pinned all-target Rust
compilation passed in 15.83 s; no unit tests executed. Runtime qualification
of this scheduling repair remains open.


### 10.126 Pause/resume preserves the attached manual quality request

Exact-source Chrome `63e56e3ef` passed both sequential manual changes,
including the repaired return to 1080p. The following rapid-choice experiment
failed after pause/resume: the pending target was superseded and the incumbent
was restored. Pause and resume each advanced the shared control generation,
invalidating the manual quality callback despite the retained attachment and
saved preference. The cold pending-quality seek was not reached.

Transport pause/resume now carries forward only an unsettled manual continuous
request from the immediately preceding generation, bound to the exact adapter,
media attachment and incumbent session. Other commands retain their existing
fences; automatic trials still cancel. The adapter and attachment are captured
when the continuous request starts. Authored regression
`pause and resume preserve only the attached manual continuous quality intent`
covers repeated transport changes, seek fencing, stale generations, adapter /
attachment / session replacement, settled and legacy requests, and automatic
trial cancellation. JavaScript syntax checks passed; no unit test executed.
Runtime qualification of this repair remains open.

The previous End census found no FFmpeg children five seconds after End,
but did not prove that the daemon remained alive. That result is incomplete
cleanup evidence. The next probe must verify the daemon identity throughout
that interval and preserve failed-stage snapshots before returning a verdict.


### 10.127 Initial placement blocks the fresh transport qualification

Commit `66488b13a` passed the normal hook and its exact-source Linux build
in 3m41s. Its fresh Chrome probe failed before initial playback: the local
worker exceeded the placement deadline. The trace also showed background
indexing of an older AVC fixture, so the retry used a library containing only
the MPEG-4 fixture. That retry failed the same placement deadline; background
indexing was therefore not the full explanation. Both failed receipts and
bounded daemon logs are preserved separately. Neither run exercised the
pause/resume repair or the improved live-daemon End census.

In the fixture-isolated attempt, VOD creation completed in 17,562 ms, after
the request had already returned 503. Aggregate node readings showed CPU
contention, but do not establish the cause. Bounded debug timings now identify
capability discovery, normative plan storage and rendition construction
separately. Admission, deadline and selection behavior are unchanged. The
startup delay remains under investigation; no unit tests executed.


### 10.128 Encoder captures share a bounded, revalidated attestation

Instrumented source `fde80eb27` failed initial placement again. Its capability
read took 0 ms, normative plan storage 0 ms per role, and rendition setup
2–3 ms per role. Most of the 13,922 ms VOD create preceded those phases.
An explicitly labeled warmed diagnostic issued the ordinary Activity detail
read before playback; engine attestation there took 6,784 ms. Creation still
took 10,033 ms and missed its placement deadline. Neither attempt reached
quality switching; cold and warmed failures remain separate failed receipts.

Recipe preparation captures the same configured encoder separately for the
video, AAC and companion recipes. Encoder capture now retains one attestation
and serializes concurrent captures. Every reuse checks the same exact object
identity used by production launch and publication fences. A changed object
is rehashed; missing or unreadable objects cannot return the cached digest.
The cache keeps one executable rather than growing with paths or recipes.
No deadline or admission rule changed.

Authored regression
`encoded_executable_capture_shares_hash_and_rechecks_replacement` covers
concurrent captures performing one actual hash, same-size / same-mtime atomic
replacement producing a new digest, and deletion refusing cached evidence.
The existing executable and dependency replacement regressions remain in the
final lane. Pinned workspace/all-target compilation passed in 1m05s; runtime
verification of the new capture path remains open. No unit tests executed. The transport pause/resume repair still
needs runtime evidence. Native input confirmed the Mac remains locked,
despite readable Safari accessibility state; the longer-pressure native
experiment did not start and its owned loopback server was closed.


### 10.129 A running target must reach its bounded preparation boundary

Exact-source `1301cafac` compiled locally, passed its corrected normal hook,
and built on Linux in 3m12s. The cold probe still missed initial placement:
VOD create took 15,739 ms. A separately labeled warmed-engine diagnostic
reached playback with 7,767 ms creation, but failed the return to 1080p before
rapid-choice/pause or seek. The new capture path therefore has no successful
runtime qualification yet; cold and warmed failures are preserved.

The failed target needed film tick 1680 / plan index 35, while its producer
was already running from index 10. The preparation demand outranks old GETs,
but the ordinary scheduler lets a running producer read forward across a
60-second gap. That policy suits fast copy; an optional encoded preparation
cannot spend its bounded readiness wait producing every old gap in between.

Admitted preparation demands now carry an explicit bounded-preparation fact.
When that demand wins scheduling, the producer targets its boundary directly
rather than using the copy restart horizon. A producer already positioned at
the target is not restarted again. Foreign foreground GETs retain arrival
priority and ordinary policy; old obligations remain protected, and settled
preparation restores ordinary scheduling. No capacity or deadline changed.

Authored regression
`bounded_quality_preparation_repositions_an_already_running_producer` covers
ordinary forward production, preparation reposition, repeated wake stability,
foreign priority and settlement. The actual wait-pool regression now also
checks a running target inside the ordinary restart horizon. Pinned
workspace/all-target compilation of the scheduling change passed in 21.99s;
the extended wait-pool test source is compiling through the normal hook. No
unit tests executed. Runtime qualification remains open.


### 10.130 Retain verified reserved artifacts when an encoder traverses them

Source `6b524dc4d` passed its normal hook and built on Linux in 3m17s.
The warmed sequential experiment reached the return to 1080p, but failed
continuity with a 516.6 ms callback gap. That failed receipt remains failed.
A targeted transport probe initially used an invalid empty quality-cycle;
its setup error was preserved, and the corrected probe used plain playback
as its precondition rather than repeating the sequential pair.

The corrected probe parked the presentation clock through pause and retained
the pending 720p target, which became Ready while paused. It then stalled at
15.9 s and destructively reopened, so rapid-change qualification failed and
the pending-quality cold seek was not reached. The first causal server fault
was `regenerated media differs from its reserved immutable artifact`: a
restarted encoder traversed an already cached interval with a different
rate-control history, and the guard correctly refused replacement bytes.

The sink now retains an existing materialized artifact only when its cached
bytes reverify against the exact live reservations and manifest byte count.
Those original bytes stay at the original URI. Traversing them advances the
process without another publication identity, marker credit or working-set
charge. New publication still verifies incoming bytes; missing or corrupted
reserved media fails closed. Source, engine, epoch and exact-key reservation
fences remain enforced. Authored actual sink / SQLite regression
`restarted_sink_keeps_verified_reserved_bytes_and_refuses_corruption` checks
changed regeneration leaves the original bytes, accounting and publication
identity intact, then checks corruption and deletion are refused. Pinned
workspace/all-target compilation passed in 17.16s; no unit tests executed.

The failed targeted case nevertheless completed a narrower End experiment:
owned FFmpeg children were absent at the immediate, one-, three- and
five-second samples, with the same daemon PID and executable identity verified
at every sample. This is process-census cleanup evidence, not physical audio,
display or queue-retirement qualification. The complete case remains failed.


### 10.131 Exact reserved-artifact repair is under runtime qualification

Committed source `4904742cb` passed its normal hook: catalog lint, pinned
Rust formatting, all-target Clippy with denied warnings, and 72 served-script
syntax checks. Source-only transfer to the approved isolated Linux root built
that exact commit with Rust 1.97.1 in 3m14s. No unit tests executed.

The targeted Chrome diagnostic is running rapid choices across pause/resume,
then pending-quality cold seek and a five-second End census. Its engine
attestation is explicitly warmed by an ordinary Activity read; it is not cold
startup qualification. Runtime verdict remains pending. The status page now
separates active work, latest failures, partial evidence and remaining work
so the current state can be read without the full chronological ledger.


### 10.132 Startup failures still prevent transport qualification

The first exact-source `4904742cb` diagnostic reached healthy playback but
failed a 500 ms callback gap immediately after its first frame. It therefore
did not attempt rapid choices. That failed receipt is preserved. A separate
diagnostic now preserves any failed precondition while allowing later
transport measurements only when the attached player is healthy; the whole
case can never become green by this continuation.

That follow-up failed before presentation: VOD creation completed in 8,756 ms,
then successful response publication lost its exact durable activation and
returned 503. It reached neither rapid choices nor cold seek. Both receipts
remain failed and the reserved-artifact repair remains unqualified.

Bounded debug phase timings now distinguish source fence, held-source probe,
catalog decoder planning, execution planning and encoded engine capture.
Earlier timings establish that the delay precedes rendition setup, but do
not identify which preparation operation dominates. The new instrumentation
changes no startup deadline, activation rule or media qualification threshold.
No unit tests executed.

Pinned workspace/all-target compilation of these diagnostics passed in 29.00s.


### 10.133 Rapid/pause now reaches presentation; cold seek needs a new frontier

Diagnostic source `79e099f1c` built on Linux in 3m55s. Its explicitly warmed
probe retained a failed startup precondition (516.5 ms callback gap) while
continuing transport measurements from a healthy attached player. Rapid
720p/1080p/720p choices across pause/resume reached a fresh 720p Presented
receipt at film time 13.345 s without changing session or player. Pause
parked the clock exactly at 2.771 s. No reserved-artifact regeneration refusal
occurred. This is a passed transport stage inside a failed complete case, not
current-source continuity qualification.

The subsequent 1080p request and cold seek to 900 s retained 720p. At the
forty-second deadline the original session was playing at 924.307 s, with
`Continuous target retained current`. Preparation still used the pre-seek
future frontier, and the global transport generation fenced the manual ask.
The owned End census again verified the same live daemon and zero FFmpeg
children at immediate, one-, three- and five-second observations. Physical
queue, display and audio retirement remain unqualified.

New phase timings attribute most startup work to decoder planning: catalog
2,679 ms plus execution 1,583 ms for primary video; shared audio execution
1,479 ms; companion catalog 1,577 ms plus execution 1,509 ms. Held-source
probes took 71/132/93 ms, source fences 0/2/0 ms, and encoded engine captures
0–1 ms. Decoder lookups rehash the held probe, snapshot and current path;
the diagnostic build is unoptimized. No verification fence was weakened.

An attached local VOD seek now carries only the same pending manual quality
owner. The adapter records the latest destination and its contiguous exposed
video frontier, retaining every old byte obligation. If a newer seek arrives
while preparation is pending, that obsolete destination cannot choose the
rung: a ready old ask is cancelled before preparing the newest frontier, and
a refused old ask may retry only because that seek advanced. An unchanged
destination never grants another retry. Forced reopen, changed attachment
and automatic trials retain their intent fences.

Authored production-adapter regression
`attached quality preparation follows a newer cold seek without reviving a fenced ask`
covers both old Ready and retained-current replies, exact 900 s frontiers,
backward seeks, invalid input and caller fences. The paired transport test
`local seek preserves only its attached continuous manual request` exercises
the actual seek-intent and supersession functions. JavaScript and test-source
syntax checks passed; no unit tests executed. Runtime qualification remains
open, alongside the measured initial callback gap.


### 10.134 Optimize development hashing without reducing verification

The pending-quality seek repair committed as `136ed5db2`; its normal hook
passed pinned all-target Clippy, formatting, catalog lint and served-script
syntax. Its authored unit regressions have not executed.

The measured decoder-planning delay occurs in an unoptimized development
executable. Each lookup validates the held probe, its sealed execution
snapshot and the current path, including content digests before any cached
fact is reused. Those checks remain intact. The development profile now
optimizes only the `sha2` dependency, matching the existing Argon2/Blake2
profile treatment while retaining workspace assertions and debug behavior.
Release already optimizes this dependency. This is a development-build cost
repair, not permission to publish stale identities or enlarge deadlines.

The existing regressions `a_replaced_probe_cannot_reuse_cached_facts`,
`transient_path_swap_cannot_change_the_executable_object` and
`snapshot_path_swap_cannot_change_the_executable_object` remain required in
the final lane. New runtime phase measurements must establish the effect;
no startup improvement is claimed yet, and no unit tests executed.

Pinned workspace/all-target compilation with optimized SHA-256 passed in 1m46s.


### 10.135 An identical seek frontier cannot renew bounded preparation

Development hashing optimization committed as `dfc8ea4ea` and passed its
normal hook, including all-target Clippy in 1m56s. No unit tests executed.

A follow-up tightens the seek repair: repeated notification of the same
segment-aligned destination is a no-op rather than another preparation
revision. Only a changed destination may supersede the pending frontier. The
authored adapter regression holds the second preparation exchange open,
repeats 900 s, and requires exactly the original and rebased prepares. It also
checks that fenced owners restore the incumbent instead of retrying the new
rung. Source and test syntax pass; combined committed-source runtime evidence
is still pending.

The follow-up hook initially failed while writing compiler metadata because
the Mac had 195 MiB free. Two obsolete owned incremental caches last used at
05:08–05:09 on October 2 were removed (about 5.5 GiB), retaining current warm
caches, source and all runtime evidence. This was a compiler-storage failure,
not a failed unit test; the normal hook is being retried.


### 10.136 Rebased cold seeks must retire the old network loader too

Combined committed source `40565116d` built on Linux in 5m11s. Optimized
SHA-256 reduced the ordinary Activity attestation from about seven seconds
to 497 ms. Primary catalog/execution planning took 151/50 ms, soundtrack
execution 48 ms, and companion catalog/execution 52/49 ms; complete initial
VOD creation took 798 ms. Every executable rehash remains enabled. This
measures the development-profile repair, not cold attestation qualification.

Rapid/pause again completed in the original session. The cold seek now
prepared the requested 1080p at exact tick 21600 / 900 s under transaction
revision 6, but no target video appended. HLS remained in
`FRAG_LOADING_WAITING_RETRY` after an old-rung load lost authorization. At
the local-seek deadline it reopened and only then presented 1080p at 900 s.
The complete receipt remains failed: initial callback gap 516.7 ms, session
replacement and two VOD creates. Recovered playback is not a passed seek.

The adapter now resets HLS network controllers with the shipped SDK's public
`stopLoad` / `startLoad(currentTime, true)` after an in-flight quality choice
spans a seek. The `true` parameter skips a second media-element seek. The
MediaSource, SourceBuffers and old byte obligations remain attached; there is
no buffer flush or session reopen. Fenced choices do not restart loading.
The latest preparation destination is also published before transport demand,
closing the interval before scrub coalescing; the actual local execution
rechecks the same frontier without renewing its revision. Duplicate frontiers
recompute current contiguous exposure but cannot grant another preparation.

The existing authored adapter regression now asserts a single network restart
at 900 s without an element seek, and no restart for a fenced owner. The
transport regression checks the destination is recorded before demand is
reported. Production and test-source JavaScript syntax checks passed. No
unit tests executed; exact committed-source runtime qualification is open.


### 10.137 Current transport stages pass; initial callback gap remains failed

Committed source `a5476334f` passed the normal hook (26.15 s) and the
source-only Linux build (3m04s). Its targeted runtime receipt retained the
failed steady precondition rather than turning recovery into a green result:
initial frame-callback gap 500 ms against the 250 ms bound. Diagnostic
continuation then completed rapid choices across pause/resume and the pending
1080p cold seek to 900 s with empty stage error lists. Both stages retained
session `c767311b-fadf-40d9-a681-d7c5a9a03e22` and player generation 3; the seek
presented at its destination in approximately 3.134 s. This qualifies those
measured transport operations only, not the complete case or physical output.

End census verified the same owned daemon executable before and after End.
Two children before End became zero at the immediate, one-, three- and
five-second samples. This is FFmpeg process retirement evidence, not device
queue, display or audible-output retirement. The Activity attestation was
explicitly warmed before playback (767 ms). No unit tests executed.

Native Safari's four-minute generated autonomous fixture now supplies an
actual pressure downgrade: initial 720p, then 1080p, then 720p at film time
42 s. The trace ended at 240.004 s with 5,760 frame callbacks, a 100 ms maximum
callback gap, one video track, one audio track and no media error. Waiting
occurred only at startup; the network stalled event followed end of playback.
The low link was 2,289,996 bit/s, derived from the actual fixture envelopes.
All media requests finished before the planned restoration at 220 s, so no
restorative upgrade was measured. This remains fixture mechanics evidence;
production native Auto enrollment and physical display/audio are unqualified.
The Mac was unlocked and native input succeeded.

The native experiment tab triggered the user's Live TV page's existing
background-stop behavior. Both owned tabs and the fixture server were closed.
An attempted restoration stopped when a fresh UI observation showed the user
had navigated to their TV library; further automation must not override that
navigation. Future native experiments require a separate owned window.


### 10.138 The full current-source run exposed startup publication refusal

The fifteen-manual / five-real-Auto campaign on `a5476334f` failed before its
first frame: VOD creation completed in 553 ms, but the final create response
was refused because exact activation publication did not settle. The ordinary
Activity read had warmed attestation (356 ms). The failed receipt is retained
as `continuous-chrome-mixed-auto-linux-a5476334f-current-transport.json`; no
switch or Auto result is claimed from that run. Owned runtime cleanup finished.

The replicated Store and HTTP publication boundary now report bounded refusal
facts: route state/publication boundary, claim state and identity-match booleans,
and exact recipe/response equality. No media bytes, credentials or serialized
recipes are logged, and no predicate, deadline, admission or recovery rule is
changed. The next committed-source probe must identify the failed predicate
before another long campaign. No unit tests executed.


### 10.139 Steady observation origins and native restorative upgrade

The earlier 500 ms steady callback receipt spans the startup preload frame
at browser time 4921.2 ms to the next callback at 5421.2 ms. Its steady start
already had a running media clock at 0.013 s but retained the preload callback.
The harness now starts the steady frame window atomically after startup,
clearing its maximum and clipping only the interval crossing that explicit
origin. A later blackout and an open interval with no subsequent callback
remain measured. Quality switches retain their full outgoing-frame interval;
no threshold changes. The old failed receipts remain failed. Authored
`steady frame window excludes preload time but preserves later and switch gaps`
covers startup clipping, later blackout, switch preservation and absent/open
callback evidence. Syntax checks pass; no unit tests executed.

The separate owned Safari recovery window completed the generated four-minute
fixture with initial 720p, 1080p at 0.792 s, a pressure downgrade to 720p at
52 s and restorative 1080p at 80 s. The trace ended at 240.004 s with 5,755
callbacks, 68 ms maximum callback gap, one video and one audio track, and no
media error. Its measured link returned high after 60 s while requests were
still active. This is native autonomous mechanics evidence, not production
adapter, physical display or audible output qualification. The owned window
and server were closed; the user's two tabs and TV-library navigation remain.
Receipt: `native-safari-autonomous-pressure-recovery.json` in the ignored
owned report directory.


### 10.140 Integrate current main and keep native policy ownership explicit

Authoritative main advanced from `15e36f7f4` to `2abd65f47`. Its GPU-resident
VA-API/Vulkan tone-map path, telemetry writer registrations bound to Store
lifetimes, native web startup handover timing and associated regressions merged
without conflicts. Current manual/Auto runtime still identifies `9b1dd149b`;
it cannot qualify this newer integrated tree. Pinned workspace/all-target compilation and the normal
hook passed before this integration was committed. No unit tests executed.

Three bounded diagnostic starts used daemon `331dc37cb` with the corrected
steady harness. All passed, with no waits, stalls or hitches; the first measured
66.7 ms maximum gap and zero owned FFmpeg children through five seconds after
End. These receipts identify both daemon and harness scope. The earlier exact
activation refusal remains unresolved; the longer current-source run retains
its bounded diagnostics.

Native Auto has an explicit design conflict: the native quality design says
to stop if AVPlayer chooses its own rung, while this plan permits a bounded
autonomous adapter with one policy owner. The human was asked to choose between
retaining Plurx policy/prepared handoffs and delegating only the admitted
compatible two-rung attachment to the native engine. Pending that choice,
the existing policy and prepared path remain; the Safari fixture results do
not silently change production policy or claim native enrollment complete.


### 10.141 Require advancing playback before a quality campaign

The `9b1dd149b` campaign completed fifteen manual changes in one attachment,
with zero waits, reopens, stalls, hitches or dropped frames. Its first change
failed the callback bound at 516.6 ms; the other fourteen measured at most
66.8 ms. The campaign stopped before Auto and remains failed. Its first
request began at film time 0.017 s, while the harness accepted a saved preload
callback at film time zero as the outgoing frame. The quality baseline now
requires a fresh callback with positive media time while playing and not
seeking before issuing the request. The complete interval from that callback
remains measured; no switch gap is clipped and no threshold changes. Authored
`quality baseline requires an advancing outgoing frame rather than preload`
covers the preload, stale callback, paused and seeking cases. Syntax checks
only; no unit tests executed.

The integrated `58bc56035` candidate is building on the approved isolated Linux
node after the older daemon and producer monitor ended. The old receipt does
not qualify this integrated tree. Producer census is preserved in the ignored
owned report directory. Final adversarial review and the Fable stop remain
pending.


### 10.142 Exact-source focused manual and actual Auto qualification passes

The focused `c8484d19b` campaign passed three manual changes followed by five
actual Auto changes: 1080 → 720 → 1080 → 720 → 1080, with the lower link derived
from the real catalog peaks and restored only after actual presentation.
All eight transitions measured at most 66.8 ms callback gap; all Auto
transaction/removal checks passed. There were zero stalls or hitches and the
final media clock measured 1.001x. The complete run took 648.706 s. This is
browser video evidence, not physical display or audible output qualification.
Receipt: `continuous-chrome-mixed-auto-linux-c8484d19b-focused-baseline-auto.json`.

End was measured while the same owned daemon remained alive: two FFmpeg
children before End, zero at the 0/1/3/5-second samples, with exact daemon
identity checked each time. The bounded producer census is retained as
`continuous-chrome-mixed-c8484d19b-focused-baseline-auto-census.jsonl`.
The earlier fifteen-manual receipt remains failed; it is not relabelled green.
The focused pass resolves the preload baseline defect but does not replace the
full current-source browser/operation matrix or intermittent startup evidence.

The committed continuous-suite CLI is now running twenty manual changes using
owned Linux Firefox, the generated AVC/shared-AAC fixture and daemon
`c8484d19b`. Chrome and its isolated daemon closed before Firefox started.
No units executed. Final adversarial review has not started; the human's Fable
stop remains immediately after that review and before fast-lane units or merge.


### 10.143 Preserve failed Firefox diagnostics and commit mixed qualification tooling

The first Linux Firefox CLI lost its connection without writing a result,
leaving an owned daemon and browser behind. Its retained daemon log shows
`Disk quota exceeded (os error 122)` during producer writes at film segment
324, followed by storage self-fencing and 503s. This is unqualified infrastructure
failure, not a passing playback receipt. Bounded daemon/browser log tails and
an explicit orphan diagnostic were preserved; exact process identities were
verified before termination. Only the completed owned runtime, its browser
profile and the older completed mixed-campaign runtime were removed, freeing
about 1.2 GiB. The warm compiler and generated fixtures remain.

A new supervised Firefox run persists status and results independently of
SSH/tool lifetime, has a forty-minute deadline, and finally cleans only its
own process group and job runtime. It runs the committed twenty-manual AVC
suite on daemon `c8484d19b`. Its result remains pending; no units executed.

`scripts/continuous-quality-qualification` makes the mixed campaign reproducible
from committed code instead of a private temporary harness. It uses the existing
lab and generated MPEG4/shared-AAC fixture, fifteen manual changes and five real
Auto changes (or three manual changes for a focused diagnostic), a 1920×1200
browser surface, catalog-derived pressure bounded between the two actual rung
peaks, exact build identity, sampled removal evidence and alive-daemon End census.
The complete mixed p95 must remain at most 100 ms and every Auto maximum at most
250 ms. Unsupported pressure intervals fail; no scripted height impersonates
Auto. Startup owned FFmpeg probes settle for eight seconds and ordinary Activity
attestation is explicitly recorded as warmed. Physical display/audio remain
unmeasured. Syntax checks pass; authored regressions are unrun.

The lab now skips the copy-fragment indexing wait only when the selected known
video codecs cannot use the AVC/HEVC copy index. Missing scan entries and unknown
codecs fail rather than become false evidence. It records this as not required,
not a completed index pass. Shaping receipts use the actual supplied five-stage
profile. These are qualification-tool changes, not production policy changes.

After the pinned source-only compiler loop builds the exact committed archive,
set `PLURX_BUILD_REF` to the reference used for that build and run on the isolated
Linux node (archive extractions carry no `.git`):

```sh
node scripts/continuous-quality-qualification \
  --server target/debug/plurxd --source-ref "$PLURX_BUILD_REF" \
  --browser firefox --firefox ../browsers/firefox/firefox \
  --geckodriver ../browsers/geckodriver --fixtures ../fixtures \
  --json ../reports/continuous-firefox-mixed.json
```

Use `--manual-count 3` for the focused eight-change diagnostic; the default
fifteen manual plus five Auto changes is the full twenty-change campaign.
Run one browser case at a time. Final main-ready adversarial review and the
subsequent human-requested Fable stop remain ahead.


### 10.144 Retain the completed Firefox failure and prepare physical iOS evidence

The supervised `c8484d19b` Firefox run completed all twenty manual switches
in one VOD session, with zero waits, reopens, stalls or hitches. It failed
the unchanged p95 video-gap limit: 133.66 ms against 100 ms. Changes eighteen
and nineteen measured 183.36 ms and 133.66 ms; the other changes were at most
84.02 ms. The run recorded 23 dropped frames out of 28,617 and a 1.001x media
clock. No disk-quota error occurred. Its persisted JSON, JUnit, console log and
supervisor status are retained, and its owned runtime was removed. This failed
receipt is not a pass.

The lab now retains bounded maximum-gap endpoints for each manual switch:
film timestamps, callback and expected-display timestamps, presented-frame
counts, attachment identities and the dropped-frame delta. These observations
will distinguish delayed callbacks from missed presentations without changing
measurement origins, scoring or thresholds. JavaScript syntax checks pass;
no units executed.

A separate signed `tv.plurx.cq.qual` / Plurx CQ Lab build 206 is installed on
17promax. Its app identity has no shared keychain access group. The production
app and its settings were not modified. The exact `b30f09ec9` Mac daemon built
with Rust 1.97.1 for isolated generated MPEG4/AAC playback. Two launch attempts
reported the phone locked, including a fresh retry after the human said it was
unlocked. No native first frame or physical output is claimed. Keep the phone
unlocked through isolated server preparation before retrying. Final review,
the requested Fable stop and fast-lane tests remain ahead.


### 10.145 Focus late-window diagnostics without replaying earlier switches

The committed mixed qualification command accepts `--start-seconds` only with
`--manual-count 3`. It uses the ordinary playback resume position in milliseconds
across browser drivers, refuses nonfinite, negative or beyond-fixture positions,
and records the requested start in the receipt. For example, start at film 990
seconds to investigate the late Firefox window with three manual and five actual
Auto changes. This is a focused diagnostic, not a replacement for the full
twenty-change campaign. Default campaigns still start at zero. Unknown dropped
frame counters remain unknown rather than being reported as zero. Syntax checks
pass; no units executed.


### 10.146 Separate focused Firefox backward-frame evidence from gap evidence

The first focused `8e722ca14` attempt omitted the node's `PLURX_BOUND_FFPROBE`
setting. It failed before creating a VOD session; its receipt is retained and
is not playback evidence. The corrected operational launcher uses the existing
verified `/tmp/plurx-cq-cd9bb38ad/ffprobe` with unchanged committed source.

That corrected run completed three manual changes from film 990 seconds in one
session. Gaps were 67.36, 83.82 and 84.26 ms, so the earlier p95 failure was
not reproduced in this focused window. The run still failed: the production
hitch detector recorded two 333 ms backward steps, at 990.54 and 1053.42
seconds. Auto was not executed after the manual failure. No waits, reopens or
stalls occurred. End had one zombie child at the immediate sample and zero
children at one, three and five seconds on the same living daemon. The receipt
and supervisor status are retained and its job runtime is removed.

The independent lab frame observer now keeps at most sixteen backward-frame
pairs, including metadata timestamps, presented-frame sequence and live element
position/state, and includes the relevant pairs in each switch receipt. This
will check whether the continuous subscription agrees with the production
detector's renewed subscriptions. It changes neither hitch criteria nor
presentation authority; no browser defect or production root cause is claimed.
Syntax checks pass; no units executed.

The Mac daemon cannot establish a production descriptor-bound decoder identity:
that implementation supports Linux production images. Physical phone playback
therefore uses a fresh exact-source Linux daemon and the generated fixture,
through an owned SSH forward and the lab's separate LAN proxy. Its process has
a twelve-minute deadline and verified identity at cleanup. Browser and native
cases run serially on that node. The native unlock window remains bounded;
physical playback has not been observed.


### 10.147 Confirm the backward step and isolate quality requests from steady playback

The `2c4240c5e` observer comparison completed three manual changes at 67.30,
83.62 and 83.92 ms maximum gap, with no independent backward-frame pairs in
those intervals. Its first actual Auto upgrade presented 1080p, but the Auto
window failed on one backward step while the incumbent was still 720p. Both
independent subscriptions recorded the same pair: 1097.041666 to 1096.708333
seconds, presented-frame count 2572 to 2573, same element, session and 720p
dimensions. The live element position was 1096.687129, without pause or seek.
This rules out a disagreement between the two observers; it does not establish
the media/decoder root cause or physically calibrated display continuity.
Maximum Auto callback gap was 84.02 ms; later Auto stages were not run. End
retired both children by one second, and its runtime was removed. The receipt
remains failed.

The mixed command now offers a bounded `--steady-seconds 1..300` control at
manual 720p, with no quality requests or Auto stage. It refuses simultaneous
manual-count selection, records its diagnostic scope and applies a zero-hitch
threshold. Combined with `--start-seconds 990`, this checks the same film
window independently of quality-change work. It cannot replace switch
qualification. Default mixed campaigns and their thresholds are unchanged.
Syntax checks pass; no units executed.

The Linux-backed physical phone launch remained locked through its bounded
window. A blocked launch receipt and bounded logs were preserved; its own
daemon, forward, LAN proxy and runtime were removed. No native playback is
claimed. The installed separate lab app remains for the requested device
qualification; production app settings and credentials were untouched.


### 10.148 Pin the retained HLS choice across startup and network resume

The `b4a2db82d` manual-720 steady control failed before its first frame:
`Unreserved superseded video load`, with one created session and no media
buffers. No steady-playback result is claimed. End retired both owned producers
by one second, and the receipt and cleanup status are retained.

The vendored hls.js `loadLevel` setter updates its manual level but leaves an
already seeded `startLevel` intact. `startLoad` can consequently select that
stale seed at initial startup or network-only resume. The continuous adapter
now explicitly sets both `startLevel` and `loadLevel` to its reserved primary
before initial loading, and updates both when a retained quality request
changes the target. This preserves the existing owner, future-load boundary
and media attachment; it does not relax an unreserved-load refusal or replace
the source. The failed control's root cause is not declared resolved until
the exact-source runtime rerun completes. The independent backward-step
failure remains open.

Authored, unrun regressions in `tests/web/continuous-adapter.test.js`:

- `continuous startup overrides a seeded start level with its reserved primary`
- `future loader resumes its retained quality rather than a stale startup level`

JavaScript syntax checks pass; no units executed. Final adversarial review
and the human-requested Fable stop remain ahead.


### 10.149 Preserve the failed retained-level rerun and identify its loaded rendition

The exact `56538a7ab` rerun still failed before its first frame on
`Unreserved superseded video load`. Pinning both HLS levels removes the stale
seed hazard, but has not resolved this observed startup failure. The failed
JSON, JUnit, console and supervisor receipts are retained. End retired both
children by one second, and the runtime was removed. No steady-playback or
media-timestamp result is claimed.

The refusal now reports bounded loaded/retained heights, fragment film tick
and the current HLS level indices. Lab snapshots record the controlled HLS
start/load/current/next indices and at most thirty-two level geometries, without
resource URLs or credentials. This identifies a wrongly selected level versus
a mismatched advertised rendition while retaining the original refusal. The
independent backward-step failure remains open. Syntax checks pass; no units
executed.


### 10.150 Preserve manual quality authority during HLS error recovery

The exact `2237f12c1` steady control failed before its first frame. Its
reserved primary was 720p, but hls.js attempted 480p with start level 1 and
load level 0. The library defaults to clearing its manual level during
fragment error recovery. The controlled constructor now explicitly preserves
that manual level: Plurx remains responsible for choosing and reserving every
rung. The original primary preparation or delivery refusal is still under
investigation; this authority fix does not claim successful startup.

The adapter retains the last sixteen timestamped refusal observations so
subsequent errors cannot erase the first refusal. Bounded steady controls use
debug daemon logging. The failed control preserved thirty-two generated
init/fragment objects (12,571,032 bytes) for offline diagnosis; those copies
have not been matched to publication identities and are not qualification
evidence. End retired its children by one second and its runtime was removed.

Authored, unrun regression:
`tests/web/continuous-adapter.test.js::controlled HLS errors preserve Plurx quality authority`.
It exercises the shipped constructor configuration and the vendored error
controller. No unit tests have run. Physical iOS launch is being retried after
the human unlocked 17promax. Final adversarial review and the requested Fable
stop remain ahead.


### 10.151 Manual authority holds; first reservation mismatch remains

The exact `9ca90cf0f` steady rerun retained start/load/next level 1 (720p),
with no unreserved 480p fallback. It still failed before the first frame.
The bounded observation history now identifies the first refusal as
`Continuous fragment is not reserved`, preceding the later 503. Video load
frontier advanced to film tick 23904/24; no SourceBuffer was created. The
scheduled transaction had no append or presentation. This isolates an
authorization mismatch instead of attributing startup to the library fallback.

The refusal diagnostic now names track type, actual tick range, byte length,
matched pin shape or absence, and two recent same-rendition pin ranges.
Identity checks and refusal behavior remain intact. The failed JSON, JUnit,
console, supervisor and bounded generated-media copies are retained. Its
runtime was removed. Offline inspection of the previous control's captured
video objects found increasing sample PTS and contiguous two-second ranges;
those startup copies do not explain the later Firefox backward step.

The fresh physical iOS launch first hit a remoteService XPC connection error;
a bounded retry then ended Locked without confirming a CQ Lab process. Its
server/proxy/runtime were cleaned up. The phone must remain awake through
launch. Physical playback remains unmeasured; production was untouched.
No units ran, and final adversarial review has not started.


### 10.152 Integrate current main startup and owned-frame seek repairs

Main advanced from `2abd65f47` to `3197d0c58`. The integration retains
upstream's `preferManagedMediaSource:false` alongside continuous loader
configuration and manual authority. Local VOD seeks both notify the
continuous reservation adapter and execute upstream's owned-frame seek
observation. The two overlapping lines are combined rather than dropping
either owner. Upstream also updates the vendored HLS seek loader lifecycle
and bounded storage/startup work. The current `4f70eabc6` diagnostic run
remains a pre-integration receipt; it will not qualify the combined source.
No units are run during this integration.


### 10.153 A steady 720p control reproduces the Firefox backward step

The pre-integration `4f70eabc6` control started in 3056 ms and played for
120 seconds without any quality request, keeping HLS start/load/current/next
level 1 and the same attachment/VOD session. No refusal observations were
recorded. Its clock advanced 119.995 seconds (1.000x), with zero waits,
reopens or stalls and one dropped frame out of 2883. It nevertheless failed
on one backward frame step: film 1032.666666 to 1032.333333, 50.4 ms apart,
with presentedFrames advancing 1027 to 1028 and actual currentTime
1032.324258. The video was neither paused nor seeking. Both observers agree.

This demonstrates that the 333 ms Firefox fault can occur without a quality
change; it does not identify its underlying decoder, media or clock cause.
The strict failed receipt remains failed. End observed no owned FFmpeg
children at each alive-daemon 0/1/3/5-second sample, and the supervisor
removed the runtime. JSON, JUnit, console, supervisor and bounded generated
media copies are retained. Current-main integration `4489dbf2a` passed the
pinned workspace/all-target Clippy, formatting, catalog and served-script
hook; its exact-source Linux rebuild and steady control are underway.
The next control also retains at most 256 KiB of its owned browser log for
backend diagnosis. No units ran, and the final review/Fable stop is ahead.


### 10.154 Keep completed publications immutable before reservation

The exact current-main `4489dbf2a` control failed at startup with a missing
AAC artifact identity for ticks 47454208–47550464/48000, 38,812 bytes. Its
ledger already contained that same tick range, so this is an artifact identity
mismatch rather than an absent timeline window. HLS retained manual 720p.
The bounded browser log has no observed audio-backend refusal. Offline
software SHA-256 inspection of three copied AAC objects agrees with native
SHA-256; this is a media diagnostic, not a unit-suite run.

The sink preserved cached bytes only if a durable quality reservation already
covered them. That leaves a window between completed HTTP publication and
Scheduled acknowledgment in which a restarted encoder can overwrite the URI
with different rate-control history. Regeneration now traverses any completed
materialized publication without replacing or recharging it. Existing reserved
identity checks still reject corrupt or missing artifacts. This closes that
static publication gap; the observed startup mismatch and Firefox backward
step remain unresolved until exact-source qualification succeeds.

Authored, unrun regression:
`crates/plurxd/src/vod/tests/chunk_03.rs::restarted_sink_keeps_published_bytes_before_quality_reservation`.
It restarts a sink before any quality pin, checks original bytes survive, and
checks no extra working-set or publication credit. No units have run.


### 10.155 Exact-source steady control passes after publication retention

The pinned Linux build of `65440c329` succeeded. Its exact-source steady
Firefox control started in 3095 ms and played for 120 seconds at 720p without
quality requests. The receipt passes: zero hitches, stalls, waits, reopens or
dropped frames, 2886 total frames, no backward observations or reservation
refusals, and 83.94 ms maximum callback gap. The media clock advanced
120.043 seconds (1.000x), retaining one durable VOD session and attachment.
This is the first passing focused control after the publication change; it
does not erase earlier failures or qualify the full switching matrix.

The supervisor removed the runtime and the machine receipts/media diagnostics
are preserved in the independent clone. A same-binary late-window focused
Firefox campaign is now running: three manual changes followed by five actual
Auto changes, with the original continuity thresholds and End census. No
rebuild or units were repeated between these runs. Physical iOS and final
adversarial review/Fable stop remain ahead.


### 10.156 Keep the mixed Firefox failure and add an Original control

The same `65440c329` source completed three manual changes with p95/max
83.98 ms gap, no waits, reopens or stalls and one VOD session. The campaign
failed on a backward observation at film 994.625 to 994.333333: 720p on the
same element, presentedFrames 112 to 113, actual currentTime 994.31573,
not paused or seeking. Auto was not run after that manual-phase failure.
The passing steady control does not clear this intermittent hitch. Receipts
remain failed and are preserved. A separately started process observer did
not collect valid samples before the campaign ended; no runtime ownership
coverage is claimed from it.

The committed qualification helper now accepts `--steady-source original`
only together with a bounded steady control. It uses the existing generated
long H264/AAC fixture and ordinary Original selection, records the actual
transport, and refuses unexpected continuous enrollment. This is a diagnostic
comparison without quality requests, not continuous-family qualification.
The original continuity thresholds remain intact. No units ran.


### 10.157 Original playback observations and corrected diagnostic criteria

The `f94c273ca` Original comparison served direct H264/AAC, started in 177 ms
and played for 120 seconds with zero hitches, stalls, waits, reopens or drops
(2884 frames, 84.38 ms maximum gap, 1.000x overall clock). No backward
observations occurred. Its machine receipt nevertheless remains failed: the
new diagnostic inherited VOD-only requirements, and its enrollment check
mistook the snapshot's all-null continuous object for an actual adapter.
These are harness mistakes, not playback failures. The failed receipt is
preserved without relabelling. One direct-play observation does not establish
the intermittent continuous fault's underlying cause.

Original controls now explicitly omit VOD/session/continuous requirements
while retaining continuity thresholds. The enrollment check requires a real
family identity. The lab's bounded frame observations now retain live
currentTime on both sides of a backward callback, so the next failed mixed
case can distinguish a playback-clock regression from callback metadata
regressing while the clock stays monotonic. No frame timestamp is rewritten
or fault suppressed. No units have run.


### 10.158 Bind Auto costs to continuous delivery and retry the unlocked phone

The exact `f815cf3d6` mixed Firefox case passed three manual transitions
(maximum 84.08 ms gap) and the first actual Auto 1080/720/1080 transitions
(84.00/83.84/83.90 ms), with no backward observations. Its overall receipt
remains failed: the second pressure stage selected 480p and created three VOD
sessions. The raw catalog advertised 6.16/12.16 Mb/s for 720/1080, while the
bound continuous master required 17.799734/33.799734 Mb/s. The qualification
therefore shaped the low link below the actual 720p budget.

Continuous enrollment and active-adapter catalog refresh now bind each family
candidate's cost to its video peak plus shared audio peak. Unbound routes are
preserved, the source catalog is unchanged, and a closed adapter cannot supply
stale costs. Production Auto and qualification now read the same delivery
budget; no selection threshold or continuity criterion is relaxed.

Authored, unrun regression:
`tests/web/continuous-adapter.test.js::continuous Auto selection uses bound video and shared audio delivery budgets`.
It exercises the shipped selector at 20 Mb/s, includes shared audio, and checks
catalog immutability and unrelated-route preservation. No units ran.

Following the user's new unlock notice, the separate physical CQ Lab launch
was retried against exact Linux build `f815cf3d6`. Apple CoreDevice failed to
mount/read developer disk image metadata (12040/12044/12018). No lab process
or physical playback was observed. The isolated server and proxy were cleaned
up. Device-service diagnosis continues; production app remains untouched.


### 10.159 Exact budget-fix build and continued qualification

The normal hook passed for `d9c8acba3`: catalog lint, pinned Rust formatting,
workspace/all-target Clippy with denied warnings, and 72 served-script syntax
checks. The committed branch was pushed. Source-only Linux transfer and build
verified Rust 1.97.1 and succeeded in 1m34s using the warm target. No units ran.
The exact-build failed focused Firefox case is now rerunning: three manual
transitions followed by five actual Auto transitions from film 990 seconds,
with unchanged continuity thresholds and from-start producer census.

CoreDevice recovered enough to report a connected phone and compatible,
usable host iOS developer image. The `d9c8acba3` separate CQ Lab retry then
reported Locked repeatedly and ended with CoreDevice 4000, remoteService XPC
unavailable. No lab PID or playback was confirmed. The owned native runtime
was verified removed before browser qualification began; the redacted blocked
receipt is retained. The user was asked to reconnect and keep the phone awake.

The previous failed Firefox run's census has 251 samples: 218 with one audio
and one video producer, two with one audio and two video producers, and one
with audio, video and an unclassified child. End observed one child briefly
zombied at the first sample, then zero children at 1/3/5 seconds while the
daemon remained alive. This is process evidence, not physical queue evidence.
Its earlier observer could omit interim zombies; the new supervisor checks
process comm and includes them. The retired disposable Mac Firefox profile
was removed after checking no corresponding browser process remained.


### 10.160 Android delivery budgets and the focused Firefox gap failure

Android continuous enrollment had the same raw-catalog cost discrepancy.
Enrollment and an active attachment's viewer-recipe catalog refresh now bind
family candidates to video plus shared audio peaks. A closed attachment returns
the raw catalog and unrelated candidates retain their original objects.
The existing Android compiler loop was re-established before editing;
production and test sources compiled successfully after the change in fifteen
seconds. No unit tests executed.

Authored, unrun regression:
`clients/android/app/src/test/java/tv/plurx/app/player/ContinuousVideoSelectionTest.kt::continuousAutoCostsIncludeSharedAudioWithoutChangingCandidateIdentity`.
It checks exact video/audio costs, unchanged valid recipe identities, raw-row
immutability, unrelated routes, and the shipped recovery selector rejecting
a link below the real continuous budget.

The exact `d9c8acba3` Firefox rerun completed three manual transitions in one
session, with no hitch or stall, but failed the first transition's 100.58 ms
callback gap against the unchanged 100 ms limit. The other gaps were 83.82 ms
and approximately 84 ms. One dropped frame was observed. Auto did not run
because the manual-phase receipt failed, so the delivery-budget fix is not
runtime-qualified. The failed receipt and 32 diagnostic media objects remain
preserved; the supervisor removed its runtime.

The committed helper now permits one manual transition as a focused diagnostic,
followed by the same five actual Auto transitions and unchanged continuity
checks. This narrows the next run to the failed early transition and pending
Auto evidence. It does not replace the required fifteen-manual/five-Auto full
campaign or relabel any old failure.


### 10.161 Single-transition diagnostic admission correction

The `34f6a87d7` narrowed run failed before any requested quality change:
`quality-cycle needs at least two named switches`. The qualification CLI
accepted one, but the shared lab runner still rejected it. This was a harness
mistake, not a successful transition or Auto observation. The failed receipt
is preserved and the supervisor removed its runtime.

The lab now accepts one or more explicitly named changes; it still rejects
an empty cycle, validates every requested quality, checks every transition
and applies the same continuity/session/frame/removal bounds. Full campaign
counts remain unchanged. The next exact-source focused run will exercise the
previously failed early manual transition followed by five actual Auto moves.
No units ran. A read-only device process query returned no matching separate
CQ Lab process after the failed phone launch.


### 10.162 Preserve fresh Auto evidence after Resource Timing saturation

The exact `a8110bccf` focused run passed its early manual transition at 66.4 ms
and actual Auto upgrade to 1080p at 83.96 ms, in the continuous attachment.
It then failed: Auto did not present 720p within 240 seconds. The corrected
catalog exposed 17.799734/33.799734 Mb/s costs and the shaper actually held
20.861 Mb/s pressure (19.8618 Mb/s measured peak). Auto hold telemetry still
reported a 95.470 Mb/s completed sample. The failed receipt remains failed;
no pressure downgrade or recovery sequence is claimed.

Inspection found completed transfer proof relying solely on retained browser
Resource Timing entries. A bounded isolated Firefox network diagnostic
confirmed saturation: 250 retained entries, 341 observer deliveries, and its
final request absent from retained history but present in the observer.
[Resource Timing](https://www.w3.org/TR/resource-timing/) queues observer
entries separately from the bounded retained resource buffer. This proves a
production evidence-loss vulnerability; whether it explains this failed Auto
stage remains to be qualified on the corrected code.

The player now starts a resource observer before incumbent or prepared HLS
requests and retains at most 128 recent HLS timing rows. Completed transfer
proof drains pending observer records and chooses fresh observed or retained
metadata. Cache/producer provenance, body sizes and request/time freshness
checks remain intact. No timing history is cleared or grown without bounds.

Authored, unrun regression:
`tests/playback/web-policy.test.js::completed video transfer evidence survives a full browser resource timing buffer`.
It supplies an observer entry absent from a full retained history, verifies
actual body proof, still rejects cache and producer-paced bodies, and checks
bounded eviction. No units executed.

The web compiler also found its generated config missing the effort's two
continuous assets. The config was regenerated. Queue result types now describe
returned values while serialization tails explicitly discard unused values;
player adapter/retention fields and control-generation state are declared,
and retained Resource Timing entries use their actual type. Compiler checking
removed the new diagnostics and two existing control-generation diagnostics.
`scripts/web-types --update --base origin/main` lowered the baseline from
517 to 515; no increase was accepted. The next normal commit/build and focused
Auto run will qualify these source changes.


### 10.163 · Distinguish policy choice from delayed presentation

The exact `432a8b322` focused Firefox receipt remains failed. Its one manual
change passed at 83.48 ms, and actual Auto presented 1080p → 720p → 1080p
with maximum gaps of 84.00/83.86/83.96 ms. The second pressure downgrade did
not present within 240 seconds. Its final ledger already contained appended
720p frames beginning at film tick 38400/24 (1600 seconds), while the video
was still at 1594.464 seconds. Thus the final state had selected and appended
the target, rather than simply retaining 1080p; the delay before that boundary
still needs diagnosis. Thresholds and the failed outcome remain unchanged.
There were zero recorded backsteps, stalls or hitches. End producer counts
were 2/0/0/0 at 0/1/3/5 seconds, and the supervisor removed its owned runtime.

Qualification now records bounded one-second policy/transfer/frontier samples
(up to 1200), including the first appended tick, so policy observation delay,
preparation delay and presentation delay can be distinguished. This changes
only diagnostic receipts. The sliced production-owner test fixtures also load
the new timing observer dependency. They remain unrun.

The latest unlocked-phone retry reached a fresh isolated Linux backend at
`432a8b322`, then reported Locked and ended with Apple remoteService XPC
unavailable. No CQ Lab process or physical playback was confirmed; the helper
cleaned its owned backend/proxy/runtime. No production app was touched.
No units executed, and final adversarial review/Fable handoff remains pending.


### 10.164 · Passing focused Firefox trace and current-main integration

The exact `2ebb4b5f3` focused Firefox campaign passed one manual and five
actual Auto transitions, with 84.02 ms maximum/mixed p95 callback gap, zero
stalls/hitches/backsteps, and zero owned producers at every End sample.
The owned supervisor removed its runtime. This remains a focused late-window
campaign, rather than the full twenty-change or physical-output matrix.

Its bounded trace shows the second pressure request waiting approximately
162 seconds before selection, followed by approximately 60 seconds of already
buffered incumbent video. Preparation and target append took about one second.
Fresh completed link samples remained roughly 25–28 Mb/s against the bound
33.8 Mb/s rung, but the mild counter alternated between zero and one whenever
the normal two-second HLS refill raised runway. This explains a phase-sensitive
selection delay; the earlier failed receipt remains failed. A correction will
preserve evidence/cooldown and require draining at the actual downgrade,
without treating a routine refill as recovered link headroom.

Main advanced to `342521018` with measured encoding, sealed source probing,
media body coordination, signed x264 reordering and AAC interval repairs.
Integration preserves independent shared AAC and explicit continuous AVC
color records, combines reordering flags with color publication, retains the
cached executable attestation while exposing upstream capture-at support, and
keeps phase timing around upstream single-probe decoder planning. Both sides'
authored fixture assertions are retained and will compile without execution.
No unit tests ran. Current-source full qualification will use the integrated
candidate, not the older passing focused source.


### 10.165 · Preserve low link margin across normal HLS refills

Current-main integration committed as `1de833ab2`; pinned all-target Clippy,
Rust formatting, catalog lint and served JavaScript syntax passed. The local
compiler required the new fragment-run version/composition fields and init
edit-list field in the shared AAC splitter/fixtures; these were fixed before
commit. No unit execution was used to discover or verify those type changes.

The route controller now counts consecutive fresh low-margin observations
independently of the normal HLS runway sawtooth. A mild downgrade still needs
at least two observations and a draining buffer at the moment of choice.
Recovered or invalid transfer proof clears the counter; cooldown, switch
budget and severe-pressure rules are unchanged. The bounded count saturates
at two. This removes the phase-sensitive counter reset identified by the
passing focused trace, without treating buffer position as bandwidth proof.

Authored, unrun regression:
`tests/playback/web-policy.test.js::mild route pressure survives ordinary HLS refills but only moves while draining`.
It reproduces 60/61/60-second runway across sustained fresh low margin, rejects
a move during refill, permits the next draining observation, and retains
cooldown/budget and invalid/recovered-proof reset behavior. No units executed.
The integrated corrected candidate still needs exact-source qualification.

### 10.166 · Exact-source Chrome full campaign and encoded boundary

Exact `feae4cc28` completed fifteen manual and five actual Auto transitions
(1080 → 720 → 1080 → 720 → 1080). The mixed maximum/p95 callback gap was
66.7 ms; each Auto transition measured 50.1 ms. There were no recorded stalls,
hitches, backsteps, dropped frames, reopens or waits, and one retained session.
The pressure-request delays were approximately 60 seconds on both cycles,
instead of the older focused trace's 162-second second-cycle delay. End
observed one exiting zombie immediately, then zero owned producers at 1/3/5
seconds. Its supervisor removed the owned runtime. Receipt:
`continuous-chrome-mixed-feae4cc28-full20-quota2.json` in the independent
clone's ignored playback-lab reports.

Two earlier attempts remain failed: an overlong socket path at startup, then
an owned-storage quota error during manual qualification. Neither supplies
a full passing receipt. Obsolete owned artifacts were pruned, and the unused
synthetic H264 fixture was copied locally and hash-verified before its remote
copy was removed. No system caches or unrelated files were removed.

Copied media from the actual 720p → 1080p presentation boundary at film
688 seconds decoded 48 frames on each side: PTS 686–687.958333 followed by
688–689.958333, with the expected 1/24-second join and no decode errors.
Adjacent shared AAC fragments decoded 188 frames / 192512 samples at 48 kHz
with increasing timestamps and no errors. Independent publisher binding and
physical display/audible continuity remain unmeasured; this is encoded-media
and browser-callback evidence, not physical-output qualification.

### 10.167 · Firefox failure and independent raster diagnostic

The full exact `feae4cc28` Firefox run presented all fifteen manual targets
but failed on three backward-frame callbacks. Maximum/p95 callback gap was
99.9 ms, with no stalls, reopens or waits. Auto stages did not run after the
manual-phase failure. Thirteen dropped frames were reported in the first two
changes. The failed receipt is preserved and the supervisor removed its
runtime: `continuous-firefox-mixed-feae4cc28-full20.json`.

A focused steady-720p 120-second run passed, but its initial raster probe was
wiped by the lab's page reload. It is not pixel evidence. A corrected probe
installed after browser preparation reproduced a backward callback in a
three-change diagnostic. The two suspicious callbacks reported media times
0.666666 then 0.375 seconds, while their media clocks advanced from 0.288799
to 0.328937 seconds. Both returned identical RGBA raster bytes. Independent
software decoding, compared across four downscalers, matched both readbacks
most closely to the source frame at PTS 0.333333. The diagnostic still failed
its unchanged hitch criterion; it does not relabel the full failure.

Firefox 157.0, build 20260924084938, ran headless. Mozilla's current
[callback implementation](https://raw.githubusercontent.com/mozilla/gecko-dev/master/dom/html/HTMLVideoElement.cpp)
selects queued future images when no next compositor tick is supplied. That
is a possible explanation for these observations, inferred from source that
is not the exact tested release revision. Canvas readback is not physical
display proof. The next bounded experiment uses headed Firefox on an owned
software display, without changing the player, thresholds or raw observations.
No units executed.

### 10.168 · Native attempts remain bounded and incomplete

Pinned Rust 1.97.1 built the exact `feae4cc28` Mac backend without unit
execution. Descriptor-bound decoder probing is unsupported on macOS, so two
local-backend Safari attempts stopped before browser startup. The Linux-backed
Safari attempt passed backend identity checks but timed out creating the
isolated WebDriver session. Failed receipts and cleanup are preserved.

Physical 17promax reports connected and prepared for development. An earlier
isolated CQ Lab launch failed with CoreDevice remoteService XPC unavailable;
no process or physical playback was confirmed, and its owned backend/proxy
were removed. After the latest unlock confirmation the process service was
reachable. The fresh exact-source baseline exhausted its bounded retry on Locked
responses (CoreDevice 10002 / FBSOpenApplicationErrorDomain 7). A lock-state
query reported passcodeRequired=true and unlockedSinceBoot=true. The helper
cleaned its owned backend/proxy after the failed launch. The production app and its data remain untouched. No physical
presentation, quality transition or audible-continuity claim follows yet.

The full Chrome process census contains 760 samples: 718 with one video and
one audio producer, fourteen with two video and one audio producers, and nine
unclassified startup-probe observations. No sample shows more than two video
or one audio producers. Each two-video observation is isolated to one sample;
the next sample approximately two seconds later has retired it. This bounds
sampled process overlap, not the exact onset between samples or physical
output queues. The final End receipt remains the separate 0/1/3/5-second proof.


### 10.169 · Headed Firefox focused proof and full campaign

The owned software display starts and retires cleanly. Its vendor Xvfb binary
is unchanged; a separately hashed copy relocates only the compiler-directory
string to the extracted owned xkbcomp, avoiding a system package installation.
No rendering code is modified. Package and relocation provenance are retained
in the owned cache, and final cleanup will remove it.

The exact-source three-change headed diagnostic passed. Transition callback
gaps were 85.46/85.58/85.54 ms, with zero measured transition hitches/stalls.
The raw raster diagnostic still contains one startup metadata backstep before
transition measurement; this is preserved, not treated as corrected or erased.
There were 3143 observed callbacks and two suspicious raster samples. End
producer counts were 2/0/0/0 while the daemon remained alive. Its supervisor
retired the display and removed the runtime. The focused result does not
replace the full campaign or qualify physical display/audio output.

A full fifteen-manual/five-actual-Auto headed campaign now runs against exact
`feae4cc28`, using the committed mixedCase and unchanged acceptance criteria.
The wrapper changes only headed launch and bounded diagnostic raster capture;
End proof remains the committed alive-daemon census. No units executed.


### 10.170 · Full headed Firefox failure is callback delay, not a backstep

The full headed `feae4cc28` run presented all fifteen manual targets, with
zero backward callbacks, measured hitches or stalls, and one dropped frame.
It failed the unchanged 100 ms p95 limit on a single 101.74 ms maximum gap
in change eight (720p → 1080p). Auto did not run. The bounded raster probe
observed 20100 callbacks and no suspicious timestamp rows. End observed zero
owned producers at all four samples; display and runtime were removed.

The offending interval was at film 380.833333 → 380.916666 seconds, shortly
after the request at 378.417 and before target presentation at 440.083333.
Its callback timestamps advanced 101.74 ms; expected-display timestamps
advanced 84.78 ms, while the presented-frame count advanced by two. These
separate observer scheduling from reported display timing; they do not prove
physical presentation or excuse the failed callback criterion. A focused
follow-up will investigate this interval before another full campaign.

### 10.171 · Cold start exposes activation-publication recovery race

The current-source rapid/pause/cold-seek attempt failed before its first
presented frame. None of those transport stages ran. It reported a 503 exact
activation-publication refusal: the active route had a publication boundary
about 373 seconds in the future. The isolated backend/proxy were removed.
The Mac and Linux clocks agreed; no system time or services were changed.

The owner inventory and renewal predicates excluded a starting request only
when its claim deadline was no later than its activation lease. A longer
claim could therefore admit an unconfirmed activation to the lease loop.
That loop can arm BLOCKED routes with the 372-second handoff safety boundary,
which prevents the creating request's zero-boundary confirmation/publication.
These source conditions explain a concrete race consistent with the refusal;
the corrected runtime still needs qualification.

Both stores now exclude starting requests from owner inventory and all three
renewal projections regardless of deadline ordering. Handoff arming also
atomically refuses an incarnation with a starting request, protecting against
an older inventory snapshot. Expiry/takeover predicates and confirmed-handoff
safety intervals are retained. No feature switch or readiness gate was added.

Extended, unrun regression:
`crates/plurx-core/tests/store_contract.rs::media_session_activation_prepare_settle_contract_runs_through_dyn_store`.
Its claim now outlives the activation lease, and it verifies blocked owner
inventory/renewal, refused handoff arming, retained confirmation ownership,
then successful confirmation/publication across SQLite and replicated storage.
Pinned Rust 1.97.1 `cargo check --locked -p plurx-core --tests --features
hiqlite-contract-tests` passed without executing tests.


### 10.172 · Exact activation fix build and passing transport probe

Fix `97967c6ce` passed the normal hook: catalog, pinned workspace/all-target
Clippy, Rust formatting and 72 served-script syntax checks. Its committed
source-only Linux build verified Rust 1.97.1 and passed in 2m24s. Source
archives/extractions were removed; no repository credentials or Git directory
were transferred. The batched PR remains draft, with 212 regression fields.
No units ran.

The exact-source targeted Chrome probe passed its first frame in 3806 ms,
with a 1.003x sampled clock, zero startup hitches/stalls, then passed three
rapid intents, pause/resume and pending-quality cold seek in the same session
and player. The 2.5-second pause held the film clock at 2.688 seconds; resume
presented 720p. The pending-quality seek landed at 900.019 seconds and 1080p,
with no continuous observation refusal at the landing snapshot. End showed
zero owned producers at every 0/1/3/5-second alive-daemon sample. Its owned
backend/proxy were cleaned up. This is transport/process evidence, not a full
current-source campaign or physical display/audio qualification.

A focused Mac Firefox run against that backend stopped before playback:
macOS refused its helper sandbox extension, and WebDriver reported an exited
browser. The owned profile/backend were cleaned up. Full bundle signature
verification succeeds outside the tool sandbox; the earlier sandboxed signature
check could not validate it. No browser replacement or sandbox disabling was
performed. A disposable Launch Services launch check is now boundedly attempting
normal app startup with the owned profile and localhost WebDriver connection.


### 10.173 · Normal Mac Firefox launch works; focused reopen remains open

The disposable Launch Services startup passed with a unique owned profile and
localhost geckodriver connected to that profile's Marionette port. The bundle
sandbox remains enabled. The helper verifies profile/process ownership and
retires only its own browser, driver and runtime.

The exact-source focused late-start probe began playback at 378 seconds, but
its first 720p → 1080p request timed out waiting for a committed switch and
created a second session. This failed receipt remains failed. A narrowed
read-only owner/selection trace is investigating the fallback. Its first
attempt accidentally dropped arguments in the operational exec wrapper, so
playback never began; that harness error is corrected for the new attempt
and is not a production startup finding.

All 212 PR regression fields statically resolve against source `97967c6ce`;
this validates names only and executes no tests. The phone still reports
`passcodeRequired=true` despite the unlock update, so physical playback
remains unmeasured. No final adversarial review or unit run has started.


### 10.174 · Correct family reproduces late-window switches successfully

The narrowed 720p-start trace showed the active family was 480p/720p;
1080p was outside that bound family. That failed probe is not equivalent
to the full campaign's 720p/1080p family. The focused follow-up starts
1080p at 378 seconds, then requests 720p and 1080p using the unchanged
committed playback-lab checks. Both switches passed on `97967c6ce` with
one durable session create and player generation, zero measured hitches,
stalls, waits, reopens or dropped frames. Maximum/p95 callback gap was
100 ms against the unchanged 100 ms limit. Ownership/selection tracing
confirmed the return to 1080p remained compatible with the bound family.
Its owned browser/profile/backend were retired.

This narrow pass does not replace the full mixed manual/Auto campaign or
prove physical output. The source-only backend's optional runtime lifetime
is now bounded to 60..2100 seconds for the full campaign; the default
720-second guard remains. The separate physical phone launch is retrying
boundedly after further Locked refusals; no phone playback is claimed.


### 10.175 · Phone retry refused; current-source full Firefox starts

The bounded physical CQ Lab retry ended with CoreDevice 10002 / FBS 7
Locked: the device was not, or could not be, unlocked. No lab PID or
playback was obtained. Its generated credentials were not printed and
its owned isolated backend/runtime were removed. Physical iOS evidence
remains unmeasured.

The full current-source Mac Firefox campaign now runs with fifteen manual
changes and the committed five-stage actual Auto measurement. It uses
normal Launch Services startup, the approved isolated Linux `97967c6ce`
backend, unchanged callback/continuity bounds and explicit source/decoder
identity checks. End will census actual owned Linux FFmpeg processes
while that daemon remains alive. The runtime has a 2100-second deadline;
physical display/audio output remains outside this browser receipt.
Main remains `342521018`. No units or final adversarial review ran.


### 10.176 · Full current-source Mac Firefox fails; calibrate the observer

The exact `97967c6ce` full Mac Firefox run presented all fifteen manual
targets with one session/player, zero stalls, waits, reopens or dropped
frames, but FAILED one transition hitch and the unchanged 100 ms p95
callback bound: maximum/p95 was 119.3 ms. Auto did not run. End observed
zero owned Linux FFmpeg children at all 0/1/3/5-second samples while the
daemon stayed alive. Its owned profile/browser/backend were removed.

The hitch was a backward callback: media time 325.25 while the element
clock was 324.884604, followed by media time 324.916667 while the element
clock was 324.931145. These are raw observations, not proof of a physical
backstep. The largest gap was during change ten preparation, at film
521.291667 → 521.375, before the target at 568.083333. Callback time
advanced 119.3 ms; expected-display time advanced 102.64 ms and the
presented-frame count advanced two. The failed receipt remains failed.

A bounded two-minute no-switch 1080p baseline now calibrates callback
metadata against read-only suspicious-frame raster samples. The raster
probe is installed after browser setup so reload cannot erase it; it
records at most 32 suspicious samples. This is diagnostic readback, not
physical display capture. No production code, acceptance threshold,
feature gate or unit-test timing was changed.


### 10.177 · No-switch calibration, idle capture and Mac relock

The first two-minute no-switch 1080p baseline failed a 599.44 ms initial
held-frame interval; no suspicious raster rows were captured. A second
baseline preserved the initial hold separately and measured a 100 ms
maximum callback gap after clock advance, without any quality requests.
Its first compositor hook started on the final read because the steady
operation does not poll snapshots; it was terminated without image data.
That harness mistake is corrected with a bounded startup-only clock poll.

The corrected ScreenCaptureKit attempt selected only the verified owned
Firefox process/window and DOM-derived video rectangle, with audio and
cursor capture disabled. It returned 5,220 idle samples and zero complete
image frames over 90 seconds. A separate read-only CoreGraphics query
then confirmed `screen_locked=true`. This is no pixel evidence and does
not erase any failed Firefox receipt. The full run did not monitor lock
state, so relock timing cannot be retroactively correlated to its failures.

Further headed helpers refuse a known locked Mac before launch. After
unlock, a temporary `caffeinate` assertion follows the exact owned browser
PID and has a 2100-second maximum lifetime; browser exit/cleanup releases
it. It changes no lock settings and cannot unlock the session. These are
operational measurement helpers, not production feature gates.

Independent full Linux Chrome qualification now runs against `97967c6ce`,
using the committed fifteen-manual/five-actual-Auto runner and unchanged
criteria. Its supervisor has a 1800-second deadline and removes only its
owned runtime. Staging had 4.81 GB quota remaining; no quota was changed.
All unit tests and the final adversarial review remain deferred.


### 10.178 · Current Chrome full campaign and natural tail pass

Executable source `97967c6ce` PASSED the committed full Linux Chrome
qualification: fifteen manual and five actual Auto changes, one session
and player, maximum/mixed p95 callback gap 66.8 ms, and zero hitches,
stalls, backward callbacks, drops, waits or reopens. First frame was
3.443 seconds and observed clock rate 1.001x. The 758 bounded producer
census samples contained 711 one-video/one-audio, fourteen two-video/
one-audio, twelve startup-unclassified-only and twenty-one empty samples.
End counts were 2/0/0/0 at 0/1/3/5 seconds with the daemon alive. The
owned runtime was removed; physical display/audio remain unmeasured.

A separate exact-source late-title probe PASSED: start at 1785 seconds,
1080p→720p presentation at 1792 seconds in the same session/player,
66.8 ms maximum/p95 callback gap, and natural EOF at 1800 seconds.
End counts were 0/0/0/0. Its 21 bounded media copies were preserved
before its owned runtime was removed. Independent ffprobe inspection
found monotonic AVC and AAC through the title tail: copied 1080p packets
ended at 1799.958333 and AAC at 1799.978667 seconds without decoder errors.
AAC segment indices do not share the video two-second index clock.

The copy budget preserved only 720p segments at 1784..1787.958333;
the actual target artifact at 1792..1794 was not copied. This is not an
independently decoded current presentation join. File SHA256 matches
artifact identity where compared with appended receipts; the initial
1080p copied artifact was bound that way. Publisher binding is not
claimed for missing target bytes. Earlier independently decoded join
evidence remains labelled with its earlier source.

After the latest human unlock message, read-only checks still reported
Mac screen lock and phone `passcodeRequired=true`. The third owned-window
calibration refused the known locked Mac before browser launch, retained
its failed harness receipt, and cleaned its isolated backend. No new
native or pixel evidence is claimed. Bounded awake assertions do not
unlock either device. Native policy ownership remains a pending human
decision; public AVPlayer resolution/bitrate preferences do not promise
exact manual variant selection. No units or final adversarial review ran.


### 10.179 · Pending-target disposal and fresh playback qualification

The first scoped disposal probe FAILED its broad FFmpeg census: startup
caption-probe children remained at five seconds. Its pending snapshot also
selected the incumbent transaction too early. Neither observation proves a
playback cleanup defect. The corrected diagnostic preserves complete
FFmpeg arguments, separately identifies caption probes by both `pipe:0`
input and `0:i:0x101` mapping, and waits for the new transaction identity.
Unknown children remain counted with playback; no producer is discarded
from the raw census.

The second attempt reached a ready, unpresented target and disposed it,
but its fresh playback FAILED at 0.796x clock and 550.1 ms callback gap.
This failure remains preserved. After requesting startup activity proof
and waiting for then-current children to finish, fresh playback passed;
that third attempt still FAILED the outer harness because its one-session
expectation contradicted two deliberate playbacks. No production code or
playback limit changed.

The fourth exact-source `97967c6ce` probe PASSED with an explicit exactly-
two-session expectation. It closed a ready, unpresented 720p transaction,
then opened a distinct session. Fresh playback had 2.016-second TTFF,
1.001x clock, 66.6 ms maximum callback gap and zero hitches/stalls/drops.
Playback-or-unclassified child counts were 0/1/0/0 after the first End
and 0/0/0/0 after the final End at 0/1/3/5-second samples. Caption probes
remained separately visible in raw samples. The daemon stayed alive
during both censuses; the owned browser/backend/runtime were then removed.

This receipt does not prove daemon restart/takeover, physical display/audio
queue retirement, or the full cancellation/source-shape matrix. The failed
receipts are retained. No units or final adversarial review ran.


### 10.180 · Restart grace, legacy recovery and fresh-family enrollment fix

The short `97967c6ce` daemon restart probe PASSED only its stated short
observation: the verified owned PID changed, buffered playback advanced
from 3.533 to 9.501 seconds in the old session, and final playback-or-
unclassified producers retired. Quality scheduling returned 410. This
is buffered grace, not successful target or durable-parent recovery.

The extended probe FAILED the stronger same-parent criterion after original
runway: legacy recovery reopened a new parent, recovered after a 2.5-second
stall and presented 720p at 44.307 seconds. Its new HLS attachment had no
continuous family. Two durable session creates were observed. This is a
forced backend-loss recovery observation, not a healthy quality change.
The owned restarted daemon and browser/runtime were removed.

Source inspection found the enrollment guard accepted only players with
no session id, while forced recovery still owns the failed predecessor id
until its create succeeds. Fix `1a84ce40a` carries that exact predecessor
identity through the bounded retry sequence and permits a fresh continuous
family only for a forced recovery of an already continuous attachment.
Healthy replacements and foreign or legacy predecessor identities retain
their existing path. The old attachment is not retired before success.
The focused ownership regression is authored and unrun; there are now 213
PR regression fields. The normal pinned hook passed. Exact source-only
Linux compilation and the fresh-family runtime check are in progress.

The latest physical phone retry received Locked, then CoreDevice 4000 /
remoteService XPC unavailable. No lab process or playback was confirmed;
its own backend/proxy/runtime and generated launch file were cleaned.
Physical iOS evidence remains unmeasured. No units or final adversarial
review ran. Native policy ownership and the promotion-policy conflict
remain pending human decisions.


### 10.181 · Exact recovery fix qualifies; new full Chrome campaign starts

Exact executable `1a84ce40a` compiled on approved Linux Rust 1.97.1 in
1 minute 37 seconds. The first fresh-family recovery probe retained its
FAILED receipt: forced recovery enrolled a 480p/720p family, while the probe
requested 1080p outside that family and incorrectly demanded a continuous
1080p receipt. Three parent creates were recorded. That is not an in-family
continuous regression, nor a pass for the outside-family prepared path.

The corrected probe reads the actual advertised family after recovery. It
PASSED: playback advanced past old cached runway to 43.791 seconds at the
saved 720p choice, then its advertised 480p companion presented at 104.159
seconds on the same recovered session/player, with durable append/
presentation ownership verified. Exactly two deliberate parent creates
were observed across the forced daemon-loss recovery. End had zero owned
FFmpeg children at all 0/1/3/5-second samples while the restarted daemon
stayed alive. Its owned daemon/browser/runtime were removed. This does not
claim seamless daemon restart, physical output, multi-node takeover or a
full restart matrix. Legacy recovery remains authoritative for real failure.

The committed full twenty-change Chrome qualification now runs once on
`1a84ce40a`, with fifteen manual and five actual Auto changes and unchanged
limits. Its bounded supervisor records actual producers and copies only
canonical initialization/early segment artifacts in a 16 MiB budget to
improve first-switch publication binding. No runtime/result is assumed
from older source. All 213 PR regression names statically resolved against
the current tree; none executed. Final adversarial review remains deferred.


### 10.182 · Type-only declaration and live fast-lane policy

The post-fix JavaScript type check found one new diagnostic: the existing
`continuousQualityBootstrap` field was absent from the Player typedef.
Documentation-only `75ffc6c2f` declares its attachment-owned shape.
`scripts/web-types --base 34252101807d5785651fe81ecead9997cf3360f7`
then passed with the unchanged 515-diagnostic/83-key baseline. Normal hook
passed. TypeScript transpilation with comments removed produced identical
JavaScript to `1a84ce40a`, SHA256
`95abd5e7551dfa47034a75432429834eea2af50675caff38e62da1f4f70092ea`
for the annotated player script. The running executable remains explicitly
labelled `1a84ce40a`; no newer binary identity is claimed.

The live pipeline correction at the top of `docs/DEVELOPMENT_PIPELINE.md`
explicitly supersedes older automatic full-qualification instructions:
main-bound PRs use `.github/workflows/main-fast-lane.yml`, full CI is
manual/release-triggered, and ready state starts the lane after review.
The actual Main promotion gate requires affected fast-lane jobs and current
head/base refs. This agrees with the human's fast-lane-only direction.
No ready transition, unit execution, full-CI dispatch or final review ran.
The required Fable pause after the final adversarial review still applies.

The new-source full campaign has durably presented all fifteen manual
changes and its initial actual Auto stage. Copied canonical first-window
AVC/AAC independently decoded without errors; candidate 1080p→720p join
at six seconds advances from 5.958333 to 6.0 seconds, exactly one frame
at 24 fps. Actual appended-artifact binding awaits the final campaign
receipt; physical output remains unmeasured. The full result is pending.


### 10.183 · New-source full Chrome passes and copied target binds

Behavior source `1a84ce40a` PASSED the full unchanged twenty-change
qualification: fifteen manual plus five actual Auto stages, heights
1080/720/1080/720/1080. Maximum/mixed p95 callback gap was 66.8 ms;
zero hitches, stalls, drops, waits or reopens; one durable session/player.
TTFF was 2.669 seconds and observed clock 1.001x. End counts were
2/0/0/0 with the daemon alive. The bounded supervisor finished zero and
removed its owned runtime. Main remained `342521018`.

The 754 producer samples contained 709 one-video/one-audio, sixteen
two-video/one-audio, nineteen empty, nine unclassified-only and one
one-video/one-audio/one-unclassified sample. The unclassified process
is not retrospectively assigned a role; exact between-sample overlap
and physical output queues remain unmeasured. Fifteen canonical media
copies totaling 12,583,159 bytes were preserved.

The 720p segment covering 6..8 seconds has SHA256/artifact id
`2bb789391f7727e30f8894d016f93901caca3e71702b08e407196b1d52ffa180`,
matching the actual appended receipt at 144..192 ticks / 24. First target
callback was at 6.083333 seconds. Both copied AVC renditions decode
without errors through the candidate six-second join, with a 1/24-second
PTS step; copied shared AAC also decodes cleanly. Independent appended-
receipt binding is missing for the incumbent 4..6-second copy and AAC,
so a fully bound two-sided presentation join is not claimed. Physical
display/audio remain unmeasured.

All final reports/census/media copies are local in ignored evidence storage.
The later Player change is JSDoc only and emits identical JavaScript; the
measured executable identity remains explicit rather than relabelled.
Compiler verification of the latest committed tree and a scoped lost-
acknowledgement reservation probe are next. No units or final review ran.


### 10.184 · Exact-tree compilation and durable append acknowledgement replay

Committed source `75c9c7ba3` built with verified Rust 1.97.1 in 1m34s
on the isolated Linux compiler. The source-only archive contained neither
`.git` nor repository credentials. No units executed.

The scoped Chrome lost-append-acknowledgement probe PASSED against that
executable. After actual SourceBuffer completion, the helper verified the
exact requested intervals in the durable ledger and dropped one HTTP 200
response. The client retried the identical payload at sequence 21 and
received the identical canonical receipt, SHA256
`d7625811e3e821a22c552420f9917a68834c36370078bc054a4edf39655307c3`.
720p subsequently presented on the same session/player, maximum callback
gap 50.1 ms, zero new stalls/hitches/drops; End counts 0/0/0/0.
The owned daemon/browser/runtime were cleaned up. This is browser and
protocol evidence, not physical display/audio or takeover qualification.

Probe attempts 1 and 2 remain FAILED: their helper incorrectly required
media arrays inside the canonical replay receipt, which deliberately clears
those arrays. Actual append facts reside in the accompanying ledger. The
corrected third helper proves those exact facts before injection and checks
canonical receipt equality on replay. No production patch was needed.
Reports use stem `continuous-chrome-75c9c7ba3-targeted-append-ack-loss3`.

After the human reported unlock, 17promax was paired and reachable, but
the bounded separate lab-app retry received Locked refusals and then
CoreDevice error 4000: remoteService XPC unavailable. No lab process or
playback was confirmed; backend/proxy/runtime were cleaned up. Mac lock
inspection also still reported locked. The human is away from home, so
physical-device retries are suspended while independent work continues.
Foreground reservation pressure and broader owner/source/platform evidence
remain open. Final review has not started; the Fable pause remains.


### 10.185 · Two foreground viewers exhaust capacity and Retry retains ownership

Source `75c9c7ba3` PASSED the scoped two-viewer capacity probe, receipt
`continuous-chrome-75c9c7ba3-targeted-foreground-pressure1`. Two isolated
Chrome profiles presented the generated fixture at 1080p and 480p. The
worker reported nine software credits occupied with a 32-credit limit.
Only the isolated lab limit was then set to the measured occupied count,
nine. The optional 720p request retained the first viewer's 1080p stream
and saved `720` preference; occupied capacity remained nine of nine.
After restoring the 32-credit limit, explicit Retry presented 720p on the
same session/player. Its maximum callback gap was 66.7 ms, with no new
stalls/hitches/drops. The second viewer retained its 480p session and
advanced normally, maximum gap 94.6 ms. Its one startup dropped frame
predated pressure and remained one; zero lifetime drops are not claimed.
Both viewers' End counts were 0/0/0/0.

The raw census includes three playback-or-unclassified children and one
separately identified caption probe. This proves a bounded occupied-pool
refusal/retry with two actual foreground viewers; lowering the lab budget
is an explicit experimental intervention, not evidence for all real-load
or physical-resource-pressure scenarios. Autonomous native entitlements,
source-reader contention and multi-node takeover remain unmeasured.
Owned browser profiles, daemon and runtime were cleaned up. No units or
final review ran. Multi-owner/source/platform qualification remains open.


### 10.186 · Three-voter preflight passes; mixed ingress exposes relay stack overflow

The isolated three-voter preflight on executable `75c9c7ba3` PASSED.
Three distinct voter processes on the approved physical host retained one
logical server identity and appeared as three activity workers. All joined
processes stopped and the owned runtime tree was removed. Receipt:
`continuous-cluster-75c9c7ba3-preflight1`. This is a same-host environment
proof, not playback, multi-host networking or takeover qualification.

The first mixed-ingress playback attempt FAILED before its first frame.
Catalog/session creation was routed to voter B, while media/quality requests
went through ingress A. Voter B created media but then aborted on
`tokio-rt-worker` stack overflow while serving the authenticated relay.
The browser had no confirmed session or durable session-start event.
The three-process environment was cleaned up; failed report and bounded
node logs use stem `continuous-chrome-75c9c7ba3-targeted-mixed-owner1`
and `continuous-cluster-75c9c7ba3-mixed-owner1-node-*`.

The relay dispatcher directly embedded every resource future in its async
state. Each resource await now boxes its future, keeping that dispatch
frame bounded without increasing worker stacks or changing authorization,
resource routing, inherited deadlines or response semantics. A new unrun
regression checks actual dispatch/authenticated-handler future footprints
against a 128 KiB ceiling without constructing or polling daemon state.
Pinned Rust 1.97.1 `cargo check --locked -p plurxd --tests` PASSED
in 1m32s; it compiled test sources without executing them. Normal hook
and exact-source runtime verification of the patch are pending. No units or final review ran.


### 10.187 · Relay stack fix compiles and mixed-owner switching passes

Fix `3983c6cef` PASSED pinned Rust 1.97.1 production/test-source check
in 1m32s, the normal catalog/formatting/workspace-all-target Clippy/served
JavaScript hook in 47.6s, and the exact committed Linux build in 1m32s.
The future-footprint regression compiles but remains unrun. All 214 PR
regression fields resolve statically; no units have executed.

The fourth mixed-ingress probe PASSED against that executable. Catalog
and session creation went through voter B; twenty-one quality exchanges
and media went through ingress A. The durable session/incarnation/owner
B/epoch 1 tuple remained unchanged while 720p presented on the same
element/Hls/MediaSource/buffers/session/player. Maximum callback gap
was 88.8 ms, zero stalls/hitches/drops, one actual durable parent start
across node-local event streams. TTFF was 9.272 s, observed initial clock
0.961x. No worker stack overflow occurred, without increasing stacks.

Playback-or-unclassified End counts across all three still-live daemons
were 0/0/0/0. Raw counts were 1/2/1/1, each identified by its exact
caption-probe arguments; those children remain visible in the receipt.
All three software-credit counts were zero at the final worker snapshot.
Owned browser, proxy, daemons and runtime trees were cleaned up; helper
exited zero. Report: `continuous-chrome-3983c6cef-targeted-mixed-owner4`;
bounded node logs: `continuous-cluster-3983c6cef-mixed-owner4-node-*`.

Post-fix attempts 2 and 3 remain FAILED. The helper first assumed peer
Activity rows exposed session ids and that ingress playback events covered
other nodes; both are intentionally node-local/private surfaces. It then
incorrectly required nonzero publication time, although ordinary confirmed
activations use zero for immediate publication. Corrected proof reads the
exact durable owner tuple read-only on the isolated leader, accepts only
already-publishable active rows, and aggregates node-local start events.
No additional production patch was needed for those helper failures.

This is three logical voters on one physical host, not a multi-host network
partition drill. Owner loss/takeover, older-peer runtime negotiation, broad
source/load coverage, Firefox continuity and native physical evidence remain
open. The Fable pause after the final adversarial review still applies.


### 10.188 · Abrupt owner loss exposes surviving paused producers

The exact-source `3983c6cef` owner-loss probe FAILED before recovery.
After the passing mixed-ingress 1080p→720p switch, it SIGKILLed only its
owned voter B daemon (PID 2259620). Ten seconds later, two pre-recorded
FFmpeg identities remained: PID 2264832, birth tick 116240730, stopped;
and PID 2265374, birth tick 116241841, sleeping. The helper requested
SIGTERM and failed the retirement bound. The stopped child remained live
and was subsequently SIGKILLed only after verifying its exact birth tick
and ffmpeg identity; disappearance was confirmed. The other identity had
already disappeared. Owned runtime trees were removed. Surviving A/C
census zeros do not prove retirement of B's orphaned children.

Receipt: `continuous-chrome-3983c6cef-targeted-owner-loss1`; bounded logs
and durable diagnostics remain beside it. No takeover/recovery success
is claimed and the failed receipt is preserved.

The Linux launcher had no parent-death lifetime contract. Broken media
pipes cannot retire a SIGSTOPped producer. Linux's parent-death signal
follows the creating thread, so installing it on temporary blocking-pool
launches would also kill healthy children when those workers retire.
The fix routes Linux spawns through one persistent process-lifetime thread,
entering the originating Tokio reactor for each spawn. Pre-exec ownership
sets SIGKILL and checks the parent identity to close the fork/setup race;
failure refuses spawning. Priority failures remain best effort. Ownership
is installed before custom probe exec hooks as part of their existing early
priority setup. Other platforms retain their existing launch path.
See the [Linux parent-death contract](https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html).

A new unrun regression deliberately retires the launching caller thread,
proves its stopped child stays alive while the owner is healthy, then kills
the owner and requires the exact child's exit. Build/hook and repeat runtime
qualification are pending. No unit tests or final adversarial review ran.


### 10.189 · Parent-death cleanup passes; recovery remains failed

Fix `def84f8c5` PASSED the normal hook (1m31s), exact committed Linux
production build (2m23s), and Linux core all-target Clippy with
`--features hiqlite-store -- -D warnings` (1m12s). Test sources compiled;
no units executed. All 215 regression fields resolve statically.

The second owner-loss probe PASSED the focused crash-retirement condition:
voter B PID 2280386 was SIGKILLed and neither of its two pre-recorded
children survived the ten-second bound. Its preceding mixed-owner quality
switch also passed. TTFF was 9.159 s, observed initial clock 1.002x.

The overall receipt remains FAILED. Playback exhausted its old buffer at
21.957 seconds and did not advance beyond it within the 150-second recovery
window. A stall-restart attempt produced a second durable parent start,
but the browser still held its old public session/family with 503 schedule
refusals. Three replacement-family producers on A remained stopped after
End at 0/1/3/5 seconds. The probe never reached a healthy companion switch.
The parent-count error additionally records two starts against the original
one-parent allowance; the helper only permits a second parent after recovery
has actually been proven, which did not happen here.

Receipt: `continuous-chrome-def84f8c5-targeted-owner-loss2`; diagnostics
and bounded node logs use the same source/stem. All owned daemons/browser/
proxy/runtime trees were removed. Exact replacement producer identities
2287228/116432026, 2287229/116432028 and 2287230/116432030 were checked
again after daemon cleanup and were gone. Runtime directories were absent.
The failure does not leave live orphaned work. Recovery/create/attachment
ownership is the next investigation; no success or complete qualification
is claimed.


### 10.190 · Refused continuous starts need an explicit disposal owner

The third owner-loss trace reproduced the recovery and End failures on
`def84f8c5`. Targeted HTTP receipts show the replacement continuous start
received 503 `media_session_handoff_pending` after roughly eight seconds.
The same failed lifetime retained manual 720p. The create-retry context was
`change`, so it abandoned the retry, while the server's cancellation-independent
handoff owner retained three paused producers behind its existing response-
lifetime safety boundary. No replacement playback or family bootstrap reached
the browser. This explains the frozen old attachment and hidden family; it
is not evidence that the ownership fence may be shortened.

Receipt: `continuous-chrome-def84f8c5-targeted-owner-loss3`; selected HTTP
request/response facts: `continuous-def84f8c5-owner-loss3-network`; quieter
bounded node logs retain the create/activation path without TLS debug flood.
Crash retirement again passed. Overall recovery remains failed.

Continuous-only handoff-pending refusals now expose the exact unpublished
session's release capability, both on initial activation and same-request
replay. They do not publish the family's media or relax any safety boundary.
The web parser accepts this capability only for that code and a UUID shape.
The bounded retry owner holds it during backoff, releases it on refusal,
abort/supersession or exhaustion, and transfers it on a successful response.
The predecessor is never substituted as the cleanup target. Legacy refusals
retain their shape. A new unrun regression covers refusal, successful replay,
cancelled backoff and malformed/unbound capability fields.

Pinned Rust 1.97.1 daemon test-source check PASSED in 29.51s; served-source
and test-file JavaScript syntax checks pass. Normal hook, exact Linux build
and focused disposal qualification are pending. No units or final review ran.


### 10.191 · Terminal successors must release background handoff ownership

Source `aa2847fca` passed its normal hook, exact Linux build and Linux
all-target daemon Clippy. The scoped disposal receipt
`continuous-chrome-aa2847fca-targeted-handoff-disposal2` PASSED: three
crashed-owner children retired, the exact unpublished family DELETE settled
in 364 ms, replacement producers retired, and End raw/playback counts were
0/0/0/0. Saved manual 720p survived. Two fault-window stalls remain;
this proves disposal, not automatic recovery. The first disposal receipt
failed a helper assumption that a no-body 204 must emit loadingFinished;
the corrected helper treats HTTP 204 as completion and separately checks
physical producer retirement.

The shipped Retry action then FAILED in
`continuous-chrome-aa2847fca-targeted-owner-retry1` with
`transcode_capacity_pending`. Only two durable parents started. Exact
family disposal stopped its producers but its background handoff retained
the replacement permit through the old owner's response safety window.

The armed handoff now polls its exact successor alongside predecessor
projection. A confirmed terminal successor or changed owner ends the wait
and releases its guard. Missing rows, lookup errors and timeouts remain
unknown and do not authorize release. This path performs no publication
completion and shortens no response safety boundary. The existing
predecessor-only fast path is unchanged. An unrun regression holds a permit
while a successor is active, durably ends it, and checks prompt release
with terminal publication still blocked. Pinned Rust 1.97.1 daemon test-source compilation PASSED in 13.49s.
Normal hook PASSED (Clippy 43.30s, served JavaScript syntax clean).
Exact committed Linux build `3c5e6d93b` PASSED in 1m36s.
Focused Retry requalification is running.
No units or final review ran.


### 10.192 · Explicit Retry after abrupt media-owner loss

Exact executable `3c5e6d93b` PASSED
`continuous-chrome-3c5e6d93b-targeted-owner-retry2`. This is the repeat
of the failed explicit Retry case, not a rerun of unit suites. Three logical
voters share one physical nuc3 host. Initial creation runs through B and
quality ingress through A; the healthy initial 1080p→720p switch retains
its exact route and presentation pipeline.

Owned B daemon PID 2347373 received SIGKILL. Both recorded child identities
retired within ten seconds. The failed replacement returned its own pending
release capability and its DELETE settled in 338 ms; unpublished producers
retired. The old attachment and saved manual 720p remained intact. One
fault-window stall is retained, so no seamless or automatic recovery is
claimed.

The shipped Retry action then restored controlled 720p on surviving A,
with an active durable route and publication_ready_at_ms=0, in 14.358s.
It advanced beyond the old recorded buffer frontier. The advertised 480p
companion subsequently presented in that same recovered session/player;
maximum callback gap was 66.7 ms with no added stalls, hitches or drops.
Exactly three parents started: initial, disposed refused family, successful
Retry. End raw/playback child counts were 0/0/0/0. Initial TTFF was 8.391s,
observed initial clock 0.957x and initial maximum gap 94.0ms. Selected HTTP,
ledger diagnostics and bounded node logs carry the same source/stem.
The helper exited 0 and removed owned browser/proxy/daemons/runtime trees.
Multi-host partitions and physical display/audio remain unmeasured.

Current main remains `342521018`, already integrated. Pinned test-source
compilation, normal hook and exact Linux build passed; Linux all-target
daemon Clippy PASSED in 1m23s. A read-only cleanup check found no
owned live daemon or current-source runtime directory. No units or final
adversarial review ran.


### 10.193 · Build the real older peer before negotiation qualification

Committed main `342521018` predates the continuous catalog/session routes.
Its actual Linux executable was built with verified Rust 1.97.1 in 2m22s
from `git archive origin/main` source only. Candidate executable `3c5e6d93b`
was preserved under the same approved owned root. Committed candidate source
was then restored and exact `2d79e2e22` Linux compilation PASSED in 2m22s.
No repository metadata or credential was transferred.

The prepared mixed-version runtime uses candidate binaries for A/C and
actual main for B, with exact build/command/config assertions. The candidate
browser and ordinary ingress are on A; continuous negotiation and legacy
creation go through B. It requires an actual 404/405 from the old route,
retained initial manual 1080p, no false continuous enrollment, and a shipped
manual prepared 720p change. Its request trace contains no synthetic refusal
or response rewriting. Runtime qualification is running. No units or final
adversarial review ran.


### 10.194 · Actual main refuses the incompatible schema join

`continuous-chrome-3c5e6d93b-targeted-older-peer1` remains a FAILED harness
receipt, before browser launch. Main-build B did not become healthy:
its bounded log explicitly reports `join_incompatible`, because the
candidate join token declares schema 71/protocol 4..=4 while actual main
implements schema 69/protocol 4..=5. This is a real durable schema admission
refusal, not a fabricated HTTP response. No compatibility or migration
check was weakened. The helper removed its owned daemons/runtime tree.

The follow-up `old-server2` runtime uses separate candidate/main lab
clusters. Browser assets come from candidate `3c5e6d93b`; every API request
runs against actual main `342521018`, with its own generated lab identity.
The proxy forwards responses without body/status rewriting. This measures
new-client/old-server endpoint negotiation and prepared manual playback,
not a mixed-version cluster whose schemas cannot join. Actual schema
refusal remains separately recorded. Runtime is running; no units or final
adversarial review ran.


### 10.195 · Old-server prepared seam refuses an unproven alignment

`continuous-chrome-3c5e6d93b-targeted-old-server2` remains FAILED. The
new client received actual main's 404 continuous catalog response and
played manual 1080p without continuous enrollment. A 720p prepared offer
arrived after 4.820s and buffered, but decoded overlap proof refused
`lost-alignment` at the incumbent-frame phase: four warm callbacks,
zero bad frames, 18ms last-frame age and 88ms drift. The incumbent remained
advancing at 1080p through film 102.603s, readyState=4, with no video error
or dropped frame. Exactly two parents started, initial plus refused staged
successor. End raw/playback counts were 0/0/0/0 and owned resources retired.
The helper's original one-parent allowance also rejected the staged parent;
that does not establish recovery or seamless presentation.

The next compatibility probe preserves this optional-refusal outcome and
requires saved 720p plus the shipped explicit Apply with restart action if
the prepared seam cannot earn proof. No presentation tolerance is widened,
no native policy changes, and no production patch was made for this probe.
No units or final adversarial review ran.


### 10.196 · Old-server negotiation and explicit manual restart pass

`continuous-chrome-3c5e6d93b-targeted-old-server4` PASSED. Actual main
`342521018` returned catalog 404; the candidate web client played retained
manual 1080p without false continuous enrollment. Its optional prepared
720p offer could not earn decoded alignment and retained the incumbent
plus saved 720p. The shipped Apply with restart action then presented
720p in a new session on actual main. Exactly three parents started:
initial, refused preparation, explicit restart. The measured manual action
through restored target took 12.839s. Initial TTFF was 11.414s and clock
1.001x. End raw/playback child counts were 0/0/0/0; helper exited zero and
removed both separate lab runtimes, browser and proxy. This is explicit
restart compatibility, not a seamless prepared handoff or mixed-schema
cluster admission.

The `old-server3` receipt remains FAILED: the helper incorrectly expected
a boolean from the async UI action, which returns no boolean after starting
playback. It recorded three parents and closed too early. The corrected
helper verifies eligibility before calling the unchanged shipped action,
then judges actual presentation. No production patch or tolerance change
was made. Actual main's schema-69 vs candidate schema-71 join refusal
continues to apply. No units or final adversarial review ran.


### 10.197 · Separate callback dispatch from display estimates

Exact `2d79e2e22` focused headed Firefox runtime
`continuous-firefox-2d79e2e22-headed3-software-output1` remains FAILED:
all three targets presented, but one callback interval was 101.58ms against
100ms. TTFF was 2.739s, clock 0.998x, with zero hitches/stalls/drops and one
parent. The owned Xvfb camera completed 7,200 timestamped crop checksums over
120s; its exit was zero, while the runtime's third change extended beyond
that capture. The supervisor exited the harness with failure, retired its
display and removed its runtime. No physical display/audio or content-order
proof is claimed from a checksum crop.

The failing callbacks advanced expectedDisplayTime from 71848.72 to
71933.34ms: 84.62ms. The first callback ran at 71831.76ms, roughly one refresh
before that display estimate; the next callback arrived at its estimate.
Media PTS advanced 69.5→69.583333 and presented-frame count 1669→1671.
This accounts for the 16.96ms difference between display and dispatch
spacing. Estimated wall-clock alignment with the Xvfb sample shows multiple
crop changes through that interval, consistent with continuing rendering;
camera startup/clock uncertainty and crop semantics remain unqualified.

The [requestVideoFrameCallback specification](https://wicg.github.io/video-rvfc/)
distinguishes callback dispatch from expected display time and explicitly
permits a refresh-late callback. The qualification harness now uses sane,
monotonic display estimates with advancing frame evidence for its existing
presentation-gap bound, while retaining raw callback maxima, endpoint
records and per-stage percentiles separately. Missing/stale/wrong-clock or
nonadvancing evidence falls back to dispatch timing. Open gaps and real
compositor gaps still fail. The 100ms bound, backward-frame and drop/stall
checks are unchanged. This corrects measurement; it does not fix Firefox
rendering or retroactively pass an older receipt. Browser display estimates
remain different from physical output evidence.

A new unrun regression uses the exact captured timings, rejects a real
compositor stall, invalid/missing/backward metadata and an open blackout,
and confirms raw callback diagnostics survive. Node syntax checks pass.
Normal hook passed (pinned Clippy in 36.89 seconds and 72 served JavaScript
syntax checks); the exact committed Linux build passed in 1m31s. The
focused new-harness result is recorded below. No units or final adversarial
review ran.


### 10.198 Exact Firefox timing correction qualification and raster diagnosis

`continuous-firefox-067788318-headed3-software-output2` **FAILED** on one
backward-frame hitch and two dropped frames. All three manual targets
presented; maximum/p95 presentation spacing was 85.58ms and raw callback
spacing was 85.60ms. First frame took 2745ms, measured clock rate was
1.002x, and the same session/player retained one parent. There were no
stalls. End producer counts were 0/0/0/0, capture exited 0, and the
supervisor removed its owned display/runtime. No Auto stages ran.

At callback 20901.24ms, Firefox reported media time 18.916666s while the
element clock was 18.542809s; the next callback at 20935.42ms reported
18.583333s with element clock 18.583462s. Presented-frame counters advanced
455→456 and both frames were 720p on the same session. The raster hashes
differ. Comparing those 64×36 rasters against 30 decoded generated-source
frames with four scaling algorithms best matches 18.541667s then
18.583333s, which advance normally. Mean RGB errors remain substantial
(approximately 16–18): the actual delivered AVC segment was not preserved,
so this is diagnostic evidence only. It does not erase the hitch/drop
failure, prove physical output, or justify replacing media time with the
element clock; doing that could hide real A/V skew.

The follow-up bounded probe
`continuous-firefox-067788318-headed3-artifact-binding1` captures suspicious
rasters with the exact family and canonical append ledger, then fetches
only their corresponding immutable video segments and initialization maps.
Each copied segment must match its append receipt SHA-256 before an
independent decoder comparison. Copies are bounded to six segments, eight
MiB per segment and a 256KiB initialization map; each request has a ten-second
deadline. The early generic spool is disabled to avoid filling the budget
with unrelated AAC. The three manual changes passed, and the first actual
Auto target presented; the remaining Auto stages are still running. This
focused diagnostic does not replace the full fifteen-manual/five-Auto
campaign. Phone qualification remains paused while the human is away.


### 10.199 Reset callback diagnostics with each actual Auto stage

The new display/dispatch measurement already resets both clocks for each
manual switch. Inspection found that `measureAuto` still reset only the
primary presentation clock, so its raw callback maximum could carry over
from an earlier Auto stage. It now clears the raw maximum and endpoint
record at the same stage boundary. Presentation timing, hitch/stall checks
and all qualification thresholds are unchanged. The source-067 diagnostic
therefore retains trustworthy raw maxima, but its reported raw per-Auto
stage values are cumulative rather than independently scoped. Node syntax
checks pass; no unit tests were executed.


### 10.200 Bound the artifact diagnostic independently of Auto qualification

`continuous-firefox-067788318-headed3-artifact-binding1` ended with
`deadline_exceeded` at its fixed 600-second supervisor bound. It entered
Auto only after the three-manual result passed and logged three actual
Auto presentations before the timeout. No terminal raster/artifact report
was produced: the helper collected it after the campaign, so the budget
cutoff left no comparison evidence. This is a failed helper attempt, not a
full campaign pass. The supervisor retired the owned display, removed the
runtime and a separate exact-runtime process census found zero survivors.
Its log/supervisor receipt and the previous failed source-067 raster/source
comparison receipts were preserved in the independent clone's ignored
evidence directory.

Committed candidate `087e27680` passed the normal hook (pinned Clippy
36.59 seconds, catalog lint, formatting and 72 served JavaScript syntax
checks), then its exact source-only Linux build passed in 1m38s. The
corrected `continuous-firefox-087e27680-headed3-artifact-binding2` explicitly
uses the shipped lab's manual-only runner, saves its bounded artifact and
raster evidence before End, and retains the same 600-second cleanup bound.
It is running; full fifteen-manual/five-Auto qualification remains separate
and requires a campaign-appropriate bounded supervisor. No units or final
adversarial review ran.


### 10.201 Corrected focused Firefox run and full campaign start

`continuous-firefox-087e27680-headed3-artifact-binding2` **PASSED** its
manual-only case: three targets presented, maximum presentation and raw
callback gaps 85.58ms, no suspect raster samples and four browser-reported
dropped frames. The suspect-metadata condition did not reproduce; the
artifact index is empty, so this run provides no new raster/append identity
comparison and does not resolve the previous source-067 failure. End
producer counts were 1/0/0/0, helper exit 0, owned display retired and
runtime removed. Immediate retirement remains visible rather than being
reported as zero. Physical output is unmeasured.

The full
`continuous-firefox-087e27680-headed20-artifact-binding1` is now running on
the same exact compiled source: fifteen manual changes followed, if they
pass, by five actual Auto stages. The helper has a 1800-second supervisor
bound and exports changed diagnostic raster/artifact state every fifteen
seconds, so a later timeout cannot discard every capture. Copied video
bytes remain bounded and compared with canonical append receipts. The
software-display crop capture covers only the first 120 seconds and stays
diagnostic. No thresholds were widened, no old receipt rescored, and no
units or final adversarial review ran.


### 10.202 Full current Firefox campaign: one coalesced observation remains unqualified

`continuous-firefox-087e27680-headed20-artifact-binding1` **FAILED** the
unchanged 100ms quality-window p95 bound at 168.94ms. All fifteen manual
targets presented on one session/player. There were zero reported
hitches/stalls/backward callbacks, seven browser-reported dropped frames,
TTFF 2729ms and measured clock rate 0.999x. No Auto stages ran. End producer
counts were 0/0/0/0; the helper exited 1, retired the owned display and
removed its runtime. Periodic capture reported no error, no suspect
raster rows and an empty artifact index.

Fourteen transition-window maxima were approximately 84.94–85.68ms. The
fourteenth window (720p incumbent, requested future 1080p) contained
callback/display times 759236.58→759405.52ms, media time
756.791666→756.958333s and compositor presented-frame counts 18164→18168.
Element clocks were 756.794277→756.958671s. Both endpoints remained 720p
on the same element/session. The request began near film 750.375s and its
1080p target first presented at 812.083333s: this observation occurred
during incumbent playback, not at the rendition handoff.

The four-frame compositor-count advance agrees with the four-frame media
advance, suggesting callbacks were coalesced rather than pictures skipped.
That does not establish the timing of the intermediate images: dividing
the gap by the count could conceal an actual held picture. The independent
software-display capture covers only the first 120 seconds, so it cannot
resolve this late observation. The failure remains failed; no threshold or
hitch rule is changed. Further focused independent display evidence is
needed before repeating a full campaign.

The separate bounded target CPU/source-reader contention probe starts only
after this runtime's cleanup. Its two owned foreground viewers retain
normal pool settings; an owned competing decoder and the new 720p target
share one CPU temporarily, with per-process identity, CPU/I/O samples and
a bounded affinity restoration/worker retirement. This measures actual
decode/read contention, not physical disk pressure or shared-reader
fairness. No units or final adversarial review ran.


### 10.203 Actual target CPU/source-read contention preserves the incumbent

`continuous-chrome-087e27680-targeted-target-source-load1` produced a
**PASS** receipt. Two actual foreground viewers presented 1080p and 480p
with the normal software pool at 9 used / 32 allowed; no pool limit was
lowered. An owned competing source decoder and the new 720p target shared
one CPU. Ten samples recorded concurrent target/workload activity: over
the comparable span the worker consumed 896 CPU ticks and read 29,626,667
characters, while the target consumed eight CPU ticks and read 872,928
characters (100 ticks/second). Both physical `read_bytes` deltas were zero.
This exercises real decode/read contention, not disk pressure or
shared-source-reader fairness.

The optional target retained healthy 1080p while saved quality remained
720p. After workload retirement and target affinity restoration, shipped
Retry presented 720p in the same session/player, 60.285 seconds after the
request at the existing append frontier. Maximum observed gap was 50.10ms,
with zero added hitches/stalls/dropped frames; the competing 480p viewer
also remained continuous. End producer counts were 0/0/0/0. The remote
daemon, worker and all owned native/browser runtimes retired. The local
Node wrapper lingered after its passing report and resource cleanup; its
exact command/PID was verified and terminated, giving cleanup exit 143.
This is recorded separately from the passing playback receipt.

### 10.204 Independent encoded frame-clock instrumentation

The generated continuous MPEG-4 source now contains a versioned optical
frame clock in the video itself: a guard byte, 24-bit frame counter and
checksum, sampled from high-contrast cell centers. The small clock plane
uses [FFmpeg geq](https://ffmpeg.org/ffmpeg-filters.html#geq), whose `N`
starts at zero, then overlays it before encoding; no fonts or browser DOM
clock are required. The fixture filename changes to `clock-v1` so a cached
unclocked source cannot be reused. Fixture verification requires 24fps and
independently decodes its first 24 encoded counters.

A conservative decoder rejects ambiguous pixels, corrupt guards/checksums
and malformed buffers. Capture analysis requires an explicit time window
and keeps sampling holes, missing pixels and backward counters visible.
It reports both observed lower and conservative upper held-picture bounds;
it never divides a gap by a counter advance. Two authored, unrun
regressions cover independent on-wire pixels/corruption and actual held
pictures versus incomplete sampling. Node syntax passes. A tiny encoded
media prototype will qualify the codec/filter path before generating the
new long fixture or repeating browser campaigns. This instrumentation does
not widen the existing callback/continuity thresholds or claim physical
output. No units or final adversarial review ran.


### 10.205 Clocked media codec qualification and baseline capture calibration

Committed `65b068f77` passed normal catalog/formatting/Clippy (37.19 seconds)
and 72 served JavaScript syntax checks, then its exact source-only Linux
build passed in 1m33s with verified Rust 1.97.1. All 220 declared regression
fields resolve statically; no units executed.

`continuous-optical-clock-65b068f77-codec1` **PASSED** actual encoded
media/filter qualification: a two-second MPEG-4 1080p source and AVC 720p
companion each independently decoded all 48 optical counters, 0→47. Their
prototype media were removed. The generated source clock evaluates only a
42×2 plane before nearest-neighbor enlargement, keeping overhead small.
The versioned 1800-second fixture was then generated once (935,253,714
bytes). Decoded source-counter checks passed at 0s (0→23), 895s
(21480→21503) and 1799s (43176→43199); the temporary encoding file is gone.

The bounded
`continuous-firefox-65b068f77-optical-baseline1` no-switch calibration completed at film 750s for 45 seconds. Browser steady playback passed,
but independent capture remained **INCOMPLETE**: 21 unreadable samples
were confined to the first 171.52ms, with the first readable counter at
179.87ms. Capture had no sampling gaps, backward counters or skipped
counter values. Its 221.45ms conservative upper bound includes startup
uncertainty and cannot establish passing output continuity. End counts
were 1/0/0/0; helper exit 0 and owned display/runtime removed.

A separate baseline2 now uses a fixed 1000ms warmup after the first raw
capture packet before starting the 45-second steady window. It retains
all startup raw pixels/timestamps and counts unreadable startup samples
separately. The warmup is time based, never conditional on a passing
decoder result; no startup continuity claim is made. It requests an owned 1920×1200 Firefox
window, measures actual video/image geometry, and samples the encoded
clock's center strip through Xvfb at requested 120Hz. Raw RGB packets are
paired with their timestamp/checksum records. FFmpeg wall-clock timestamps
are preserved with a microsecond encoder time base; a tiny prior mechanics
probe confirmed their epoch domain. The final explicit browser measurement
window is compared with this independent captured counter stream. Missing
or ambiguous pixels, capture holes and held pictures remain visible; no
averaging over counter advances is permitted. Software-display pixels
remain different from physical Apple/Android display and audible output.
No units or final adversarial review ran.

Baseline2 completed with browser playback **PASS**, zero unreadable steady
pixels, no backward counters and no skipped counter values. Independent
capture remains **INCOMPLETE**: one 13.384ms sampling hole at +150.477ms
exceeded the unchanged 12.5ms maximum. Observed picture-hold lower/upper
bounds were 58.34/75.09ms. All startup evidence remains recorded (104
unreadable prefix samples); End and cleanup passed. Baseline3 now requests
240Hz capture, retaining the same fixed warmup and completeness limit.

Baseline3 at requested 240Hz completed with independent steady capture
**COMPLETE**: 10,803 measured samples, zero unreadable pixels, capture holes,
backward counters or skipped counters; picture-hold lower/upper bounds
58.37/66.64ms. The startup prefix retains 200 unreadable samples and is
excluded only by the fixed warmup policy. Browser steady playback passed,
helper exit 0, owned display retired and runtime removed. A focused
720p→1080p request starting at film 750s now uses the same calibrated
capture. It does not replace the full campaign or rescore older failures.
Current main remains integrated (`342521018`); no units or final review ran.

### 10.206 Late optical switch: wrong family attempt retained, corrected probe running

`continuous-firefox-65b068f77-optical-late-switch1` **FAILED**: the helper
started at 720p, enrolling the 480p/720p family, then requested out-of-family
1080p. The target wait timed out and two durable VOD session creates were
recorded. This repeats the known family-enrollment mistake in the diagnostic
helper; it is not evidence that a compatible family switched successfully.
Independent capture was complete over 21,630 measured samples, with zero
unknown pixels, capture holes, backward counters or skipped counter values;
its picture-hold lower/upper bounds were 71.11/79.75ms. Those pixel results
do not make the extra-session-create run pass. End counts were 0/0/0/0; helper
exit 1 and owned display/runtime cleanup confirmed.

The corrected `optical-late-switch2` starts at 1080p and requests 720p then
1080p, retaining the full campaign's 720p/1080p family. Its fixed warmup,
240Hz capture, strict completeness limit and existing browser thresholds
are unchanged. It is a focused diagnostic, not the full success series.
No units or final adversarial review ran.

### 10.207 In-family browser pass; full capture remains incomplete

`optical-late-switch2` passed browser checks: initial 1080p, 720p at film
764.083333s (12.671s request-to-presentation), then 1080p at 826.083333s
(61.643s at the existing append frontier). One durable session/player,
85.62ms maximum quality-cycle gap, zero hitches/stalls and two browser
reported drops. End counts 2/0/0/0, helper exit 0, owned cleanup confirmed.

The optical helper initially used `result.start`, which the quality-cycle
harness replaces with its final observation baseline. Consequently its
first analysis covered only the last eight seconds. Reanalysis of the
existing checksum-paired raw capture, from the fixed warmup end through
final observation, preserves the original analysis separately and does
not rerun playback. The corrected 83.024-second window has 19,925 samples,
zero unreadable/backward counters, one 12.878ms capture hole at +13.321s
and two skipped counter values at film 765.458333→765.541667s and
765.666667→765.750000s. Those skips coincide with the two reported browser
drops shortly after 720p presentation. Picture-hold lower/upper bounds
82.41/93.72ms. Independent capture remains **INCOMPLETE**, and these
results do not establish the required zero-skip switch series.

`optical-late-switch3` now records the correct full window directly and
adds bounded 25ms browser event-loop lag diagnostics to investigate the
observed skips. Thresholds, fixed warmup and playback behavior remain
unchanged. No blind full campaign was launched. Evidence/status commit
`77bbe1af3` passed the normal hook (Clippy 35.47s, 72 served script syntax)
and is pushed; running binary/source remains exactly `65b068f77`.
No units or final adversarial review ran.

### 10.208 Captured startup freeze precedes quality requests

`optical-late-switch3` passed browser checks: two in-family changes, one
session/player, 85.46ms maximum gap, zero hitches/stalls, four reported
drops; End 2/0/0/0 and owned cleanup passed. Full independent capture was
complete (19,450 samples, no unknown pixels or capture holes), but recorded
four skipped counter values and a 215.65ms lower/229.54ms upper picture
hold. Event-loop lag peaked at 25ms, giving no evidence of a comparable
main-thread stall.

The long hold and all four skips were at film 751.291667→751.583333s,
before the first quality request's recorded outgoing position 751.625s.
Both quality transitions subsequently had no captured skips. An 83.42ms
lower hold was observed shortly after 720p presentation at 762s. This does
not turn the full post-warmup capture into a healthy startup result or
resolve the older full Firefox failure.

`optical-late-switch4` now records exact browser Unix timestamps immediately
before each actual `setQuality` call. It analyzes first quality request
through final observation, and separately retains the entire post-warmup
capture and raw startup prefix. Independent switch-window failures now
fail the new helper's run: incomplete capture, backward/skipped counters,
or a conservative held-picture bound over the unchanged 100ms limit.
This is an explicitly scoped switch measurement, not retrospective removal
of failing startup samples. No units or final adversarial review ran.

### 10.209 Independent pixels identify a false backward-hitch baseline

`optical-late-switch4` **FAILED** its existing browser hitch rule (one
backward-frame hitch), although both its full post-warmup and exact request
windows independently captured zero unknown/missing/backward/skipped counters
and an 83.29ms maximum conservative picture hold. Browser drops were zero;
event-loop lag peaked at 8ms. End 1/0/0/0, helper exit 1 and owned cleanup
confirmed. The failure remains failed.

Binding the offending callbacks to timestamp/checksum-paired optical pixels
shows reported 769.125→768.791666s while captured counters show
768.666667→768.750000s, within 1.83/1.44ms of the expected display timestamps.
Nearby element-clock samples were 768.709/768.750s. The earlier callback
reported a future PTS more than 400ms ahead of the clock and captured video;
its next ordinary callback became a false backward-hitch baseline.
[The API definition](https://developer.mozilla.org/en-US/docs/Web/API/HTMLVideoElement/requestVideoFrameCallback)
distinguishes compositor submission, expected display and media PTS; these
independent pixels demonstrate the mismatch for this receipt. An older
[Mozilla issue](https://bugzilla.mozilla.org/show_bug.cgi?id=1935253) concerns
cached old frames, but is not claimed to diagnose this current mismatch.

The hitch detector now preserves implausibly future media timestamps as
bounded anomaly receipts rather than updating its valid baseline or settling
presentation from them. It allows expected future display time, playback
rate, two nominal intervals and coarse 100ms clock rounding. The last valid
frame remains intact so the next callback still spans any real hold or back
step; existing hitch/continuity thresholds are unchanged. The lab exports
anomaly count/receipts alongside ordinary faults. One authored, **UNRUN**
behavioral regression covers future metadata, expected future submission,
false-back prevention, genuine backward frames, unsettled uncertain frames,
and a real late frame across a rejected outlier. Script syntax and diff
whitespace checks pass. Runtime qualification is pending; no units or final
adversarial review ran.

### 10.210 Exact measurement-fix replay: browser pass, capture incomplete

Committed/pushed `f7a6363a6` passed the normal hook (Clippy 38.06s, 72
served scripts), exact pinned Linux build (1m33s), and static resolution of
all 221 declared regressions. No units executed. Draft #774 has the current
source, evidence limits and new regression field.

`continuous-firefox-f7a6363a6-optical-late-switch1` **FAILED** because the
independent switch window had one capture hole above its strict 12.5ms
limit. Browser checks passed with zero hitches/drops; no future metadata
anomaly occurred, so runtime exercise of the new rejection is not claimed.
The 19,903-sample optical window contained zero unreadable/backward/skipped
counters and a 91.73/99.89ms held-picture lower/upper bound. End 0/0/0/0,
helper exit 1 and owned display/runtime cleanup confirmed. The failed
receipt remains failed while capture scheduling is investigated.

Private network mechanics also passed in three ephemeral user/network
namespaces: all three echo endpoints reachable, only B unreachable after
its private bridge link was cut, all reachable after restoration. Every
endpoint and bridge retired. This proves partition tooling only, not plurx
playback, physical multi-host behavior, or cluster recovery. Host networking
and production containers/services were untouched; Docker was not used.
No final adversarial review ran.

### 10.211 Capture scheduling calibration; framebuffer comparison running

The exact f7 replay's sole sampling hole was 24.711ms at +1.061s after
its first quality request. A separate no-switch calibration pinned only
owned camera CPU 0 and Xvfb CPU 2; all browser/daemon/workload affinity and
pool limits remained unchanged. Browser playback passed, but independent
capture remained **INCOMPLETE** on three sampling holes (zero unknown,
backward or skipped counters; 71.51ms upper picture-hold bound). End and
owned cleanup passed. This unsuccessful affinity approach is discarded.

A separate bounded paired baseline now compares X11 capture with read-only
sampling of the owned Xvfb XWD framebuffer. The latter reads actual pixel
coordinates, records real sample timestamps and checksum-paired RGB packets,
and bypasses synchronous X11 capture requests. No synthetic samples or
conditional waiting for passing counters are introduced. Both methods keep
startup uncertainty, capture holes and invalid counters visible. This is
software-display calibration only; physical output remains unmeasured.
No units or final adversarial review ran.

### 10.212 Private voter bootstrap passes; framebuffer calibration remains incomplete

Three actual `f7a6363a6` daemons bootstrapped as reachable voters in separate
private network namespaces on one physical host. Cutting only B's private
bridge port made B unreachable while A/C remained reachable; restoring the
port restored all three voters. All owned daemons, bridge and runtime were
removed. This is topology/bootstrap evidence only: it has not yet proved a
write through surviving quorum or playback under partition. A bounded
quorum-write/restoration probe is prepared and will start after display
calibration retires, keeping that calibration free from this startup load.

The paired framebuffer baseline completed with both methods **INCOMPLETE**:
X11 had one capture hole but no unreadable counters; read-only mmap had no
capture holes but 103 unreadable samples while painting was in progress.
Both had zero backward/skipped counters. The mmap upper hold bound includes
unknown-region uncertainty and is not a physical hold measurement. End,
helpers and owned cleanup passed; no failed capture was rescored.

The next paired baseline requires two identical raw pixel reads within a
fixed 2ms budget, independent of decoded counter validity. An unstable pair
is recorded explicitly as unknown; a stable corrupt counter is also unknown.
Raw pixel/timestamp/checksum evidence remains retained. No conditional wait
for a passing decoded counter is used. No units or final adversarial review
ran.

### 10.213 Consistent framebuffer baseline and actual private quorum write

The consistent-pixel framebuffer baseline completed with **COMPLETE** steady
capture: 10,805 measured samples, zero unknown pixels, capture holes,
backward counters or skipped counter values; 66.89ms conservative hold
bound. Its fixed retry budget is 2ms **after the first read**; the recorded
startup prefix remains separate. All helpers/display/runtime retired.
This is calibration evidence, not physical output or the full switch series.

The actual private three-voter quorum-write probe **PASSED**: while B's
private bridge link was down, A/C remained reachable and a replicated
settings change completed in 183ms. C observed it during partition; after
restoration B observed the same committed value and all voters were
reachable. Every daemon, bridge and runtime retired. No production host
networking/settings or other containers changed. This is real private
network/quorum evidence on one physical host, not playback under partition
or physical multi-host qualification.

After that cluster cleanup, a focused exact-f7 framebuffer switch probe is
running. It records before/after read timestamps, treats any whole read over
4ms as unknown, and conservatively adds measured read-span uncertainty to
picture-hold upper bounds. Both full post-warmup and exact-request windows,
as well as the paired X11 diagnostics, remain retained. Browser checks and
independent zero-skip/backstep/unknown/capture-hole and 100ms hold limits
must pass together. No units or final adversarial review ran.

### 10.214 Bracketed switch failure is during preparation, not decoder join

`optical-framebuffer-switch1` **FAILED**: browser quality-cycle p95/max
135.26ms exceeded 100ms; X11 pixels recorded a 141.14ms lower/150.14ms upper
hold and one skipped counter. Bracketed framebuffer capture was incomplete
on 22 unknown samples and one skipped counter; its whole-read maximum was
8.027ms, over the explicit 4ms budget. Its inflated upper bound includes
unknown-region uncertainty and is not a picture-hold measurement. Browser
reported one drop, zero ordinary hitches and zero metadata anomalies.
End 1/0/0/0, helper exit 1 and owned cleanup confirmed.

The X11 hold was at film 766.541667s, about four seconds after the 1080p
request at 762.500653s, while old 720p was still presented. Actual 1080p
presentation was at 824.125s, almost a minute later. This rules out that
particular hold being the decoder's resolution-change join. Event-loop lag
peaked at 16ms. The receipt remains a real unresolved software-display
continuity failure; it is not explained away by the earlier metadata fix.

A controlled private CPU-set experiment gives browser/display/capture
0–3,8–9 and the owned backend 4–7,10–15. Its first attempt **FAILED** before
media creation (uncalibrated video geometry, zero VOD creates), so it proves
no continuity outcome. The next attempt explicitly sets a 32-credit isolated
software pool, matching earlier scoped load probes, because visible CPU
count can affect automatic budgets. It records capacity and startup body/
player/video diagnostics. This is a distinct resource-condition experiment;
no production host affinity, configuration or acceptance threshold changes.
No units or final adversarial review ran.

### 10.215 CPU-separated switch passes X11 evidence; native sampler replaces slow observer

The exact-f7 second CPU-separated probe created playback and presented both
720p/1080p targets in one session/player. Browser checks passed: 85.50ms
maximum gap, zero hitches/stalls/drops. Paired X11 switch-window capture was
**COMPLETE**: 19,430 samples, zero unreadable pixels, holes, backward or
skipped counters, 83.26ms conservative picture-hold bound.

The overall receipt remains **FAILED** because the primary Python framebuffer
observer recorded 17 uncertain samples and a 6.236ms maximum whole read, over
the unchanged 4ms observer limit. Its inflated hold upper bound includes
unknown-region uncertainty and does not measure a physical hold. The isolated
pool was explicitly 32 credits; backend/client CPU sets were separated. This
is scoped resource-condition evidence and does not qualify default conditions.
End retired the final zombie by one second; all display/runtime resources
retired. No startup-state receipt was needed because startup succeeded.

A bounded native C observer now samples the same actual XWD RGB coordinates
with volatile reads, two identical byte reads within the fixed 2ms retry
budget, before/after timestamps and checksum-paired raw packets. Its whole
read limit remains 4ms; uncertain pixels remain unknown. It compiles with
existing host libraries under `-Wall -Wextra -Werror`, requires owned input/
output paths, captures at most 95 seconds and retires on SIGTERM. The focused
paired switch probe is running; it has not yet passed. No production setting,
acceptance threshold, unit test or final adversarial review changed.

### 10.216 Direct framebuffer observer is discarded; private ingress is proved

The bounded native observer reduced maximum whole-read duration to 0.099ms,
but still recorded 17 unreadable partially painted samples. The overall
focused receipt therefore remains **FAILED**. Browser checks passed at
85.52ms with no drops; paired X11 capture was **COMPLETE**, 19,895 samples,
zero holes/unknown/backward/skipped counters and 71.81ms conservative hold
bound. End 2/0/0/0 and all display/runtime cleanup passed. Neither failed
observer receipt is rescored. Read-only direct framebuffer sampling is
discarded as an acceptance method because byte stability cannot establish
paint completion. These passes remain scoped to explicit CPU/pool conditions.

A separate private-network ingress mechanics probe **PASSED**: a host
loopback-only socket inherited into a private network namespace forwarded
to its owned internal echo service. The exact response was received and
its child retired. No production interface, service or route changed.

The next bounded probe runs shipped Firefox playback inside a wholly private
network with three actual voters. It deliberately starts on B through mixed
ingress, cuts only B's network link, observes the saved manual choice and
surviving nodes while all daemons remain alive, then restores the voter
roster. This is partition/presentation-state mechanics, not an assertion of
seamless takeover or physical multi-host/display/audio qualification. No
units or final adversarial review have run.

### 10.217 Default-condition full campaign and fresh Android runtime preparation

Status commit `9e0a32796` passed the normal hook (Clippy 35.73s; 72 served
JavaScript scripts) and is pushed to draft #774. Main remains fully integrated.
All 221 regression fields resolve statically. Its source-only pinned Linux
build passed in 1m32s, after all previous private runtimes retired.

The default-condition full Firefox campaign is running on that exact source:
fifteen manual transitions followed by five actual Auto transitions, one
X11 observer, no affinity overrides or explicit pool override. Both the full
post-warmup and exact-request capture windows must satisfy the unchanged
completeness/counter/100ms hold bounds. The owned camera lasts at most 1950s,
supervisor at most 2100s; at least 3.5GB free was required before launch.
All other lab workloads remain off this host during capture. No pass is
claimed yet. Physical display/audio remain unmeasured.

The private three-voter playback probe **FAILED** before controlled initial
1080p playback (no presented first frame); it never cut B's network link.
All owned nodes, bridge and runtime retired. It proves no partitioned
playback. The next harness revision records bounded response classes and
startup body/player state so this failure can be diagnosed before repetition.

A fresh temporary API36 arm64 Android device is being prepared on the Mac,
with separate AVD/storage/emulator home and ADB port 5041/device 5580. The
user's existing AVDs and physical devices are untouched. The branch's debug
APK assembled in 46s with `:app:assembleDebug`; no unit task was invoked.
Emulator boot is bounded to 180s and lifetime to 2100s after boot, with owned
process/server retirement. It is a software-runtime opportunity while the
human is away, not physical-device or audible-output acceptance. Playback
will wait until the Linux capture retires. No units or final review ran.

### 10.218 Quota failure preserved; captures archived and compressed in flight

The first default full campaign on `9e0a32796` **FAILED** with a Node writable
stream EDQUOT error (-122) before producing its final JSON/JUnit receipt.
Supervisor exit 1, display retirement and runtime removal are recorded;
no manual/Auto count or continuity success is inferred from that incomplete
run. Global free `/tmp` space alone did not predict its write quota.

Forty-five owned RGB/timestamp files (1,467,150,783 bytes) were streamed into
a 20,255,368-byte compressed Mac archive. Every member's byte count and
SHA-256 matched its manifest before cleanup; every remote file was rechecked
against the same manifest before deletion. The archive is retained in the
independent clone's ignored reports. Its SHA-256 is
`bda402154a698cf0442bfbf8b455f89a0912f0fc7de97b0ece2f7ab5bfd13bce`.
No failed receipt was deleted or rescored.

The separately identified `optical-default-full2` campaign is running with
lossless streaming gzip storage. Each decompressed RGB packet is still
matched to its original paired MD5; incomplete packets/counts fail. The
measurement windows, cadence and completeness/counter/hold limits are
unchanged. Capture output grows in compressed form, reducing the space
needed while retaining original pixels. No other lab workload runs on its
host.

The fresh Android emulator booted in about 27s. Debug build 144 installed and
reached server discovery; no production login occurred. The APK is
83,809,218 bytes, SHA-256
`7e7a4203dbaf7a7d49bacfc653ff35474b4f14acf29aa6399afc976cd500b70c`.
Its isolated runtime startup receipt is retained. Playback remains queued
until the full capture retires. No units or final adversarial review ran.

### 10.219 Default Firefox failure preserved; Android player construction repaired

The second default Firefox campaign on `9e0a32796` **FAILED** after all
fifteen manual targets presented; no Auto stage ran. Callback p95/max was
170.080ms with eleven dropped frames and no hitches/stalls. The independent
request-window capture had 213,429 samples, zero unknown samples, seven
capture holes and eleven skipped counter values. Its largest held-counter
lower/upper bounds were 189.148/197.614ms. Both excessive callback intervals
occurred during target preparation, before target presentation; this is not
evidence of a failed resolution-change join. All owned display/runtime
resources retired. Original compressed pixels and paired MD5 rows are
retained; this receipt remains failed.

The fresh Android emulator connected only to the isolated generated-media
backend, opened its fixture and crashed on Play before session creation.
Its crash log identifies `getBackBufferDurationUs not implemented` in
Media3 player construction. Kotlin interface delegation left Java default
methods on the wrapper rather than forwarding to `DefaultLoadControl`.
The wrapper now explicitly forwards the current Media3 lifecycle, track
selection, back-buffer, startup and preload methods, preserving its existing
allocator ownership and controlled twelve-second loading budget.

A regression covering delegated lifecycle and startup/preload decisions was
authored. Production Kotlin, unit-test Kotlin compilation and debug APK
assembly passed in 14s; **no unit test executed**. The previous owned emulator
retired at its deadline, and a separate bounded emulator run is starting for
runtime verification. Its backend remains exact compiled Linux source
`9e0a32796`; the APK contains the Android-only uncommitted fix. Physical
output/audio remain unmeasured. Final adversarial review has not started.

The repaired APK enters the player and leaves the crash buffer empty, but its
controlled initial session then refuses before media presentation. The owned
backend's health endpoint returns 200. This is a separate failure, not a
playback pass. A separately identified temporary diagnostic APK records only
bounded app class/method/line identifiers; it contains no exception message,
URL, credential or media path. Its temporary source instrumentation was
restored immediately after assembly and will not land in the PR.
