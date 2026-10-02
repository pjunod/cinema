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
| CQ2a | Integrated upstream route-v1 floor; independent cancellation envelope being built | Pinned workspace/all-target compile; negotiation and exact-cancel regressions authored, not run | Separate bounded public/peer route, exact accepted intent fences; web caller and durable cleanup receipts implemented; client replay/retention settlement remains |
| CQ1 | Integrated upstream retention/admission | Pending-planning cancellation regression authored; not run | Upstream safety reused; incumbent-wait planning gap closed; manual retention and explicit Retry/restart implemented, qualification pending |
| CQ2 | Strict transaction ledger and owner-fenced storage implemented; serving integration underway | Pinned workspace/all-target compile; lost append, replay, takeover, pin-pressure and cross-language fixture regressions authored, unrun | Dependency reservations persist; producer/cache-pin and client adapters still need integration |
| CQ3 | — | — | Not run |
| CQ4 | — | — | Not run |
| CQ5 | — | — | Not run |
| CQ6 | — | — | Not run |
| CQ7 | — | — | Not run |
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

One generation and attachment retain 16 transactions, 64 reserved intervals,
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
