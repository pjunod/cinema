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
| CQ2 | Strict transaction ledger, owner-fenced storage and serving integration implemented | Pinned workspace/all-target compile; lost append, replay, takeover, pin-pressure and cross-language fixture regressions authored, unrun | Dependency reservations, physical pins and web adapter implemented; pressure/takeover qualification remains |
| CQ3 | Verified two-rung AVC/shared-AAC family implemented | Actual isolated Linux init verification and production probes | BT.709 proof passes; continuity qualification remains |
| CQ4 | Controlled cold admission and demand retirement implemented | Pinned all-target compilation; regressions authored | Measured cleanup and pressure qualification remain |
| CQ5 | Production hls.js enrollment, reserved loader and observers implemented | Exact-source Chrome probes; first frame and first rung observed | AAC pin and logical abort fixes implemented; rate-budget fix awaits replay; full Chrome/Firefox series remain |
| CQ6 | Warm prepared surfaces and original overlap clocks implemented | iOS/tvOS and Android source compilation | Unit execution deferred; physical qualification remains |
| CQ7 | Public API and SDK constraints audited; prepared path retained | Official variant/track API documentation; device inventory | Continuous native adapters and device evidence remain unfinished |
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
