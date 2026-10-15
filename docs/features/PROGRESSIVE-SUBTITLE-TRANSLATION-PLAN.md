# Progressive subtitle translation — build English captions during playback

**Status:** parent and independent engine reviewed R1–R9 design; no translation code
is authorized to start yet · **Requested:** 2026-10-15 · **Draft base:**
`73788aa88` · **Execution prerequisite:** complete the current subtitle
usability effort and freeze its qualified interfaces first.

Companion to [offline generation](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md):
that effort installs a model and publishes complete same-language captions.
This plan answers a different question: how you watch foreign-language audio
while English captions become available progressively, without terminal setup
or restarting the picture. Work milestone by milestone. If this requires
weakening source, publication, playback-owner or resource-admission fences,
stop and review the contract change before implementing it.

## 1. The product promise is progressive availability, not guaranteed realtime

You install a compatible model in Settings, choose the actual spoken audio
and its language, then choose **Translate to English** in the player. The
picture continues. The English track initially reports **Preparing**, then
shows translated cues for committed time ranges. Original caption tracks and
Off remain available. A distinct status reports coverage ahead of playback,
missing ranges and any lag. Translation never silently replaces an explicitly
selected original track or Off.

The user selected progressive playback on 2026-10-15. Waiting for the entire
film before offering captions does not satisfy this request. A slow worker
may fall behind; the UI must say so without claiming realtime or pausing the
film. There is no evidence yet that supported workers can sustain translation
for any particular language, model, film or concurrent playback workload.

The minimum user-goal acceptance floor is at least one ordinary single-node
movie playback route with useful progressive translated captions from packaged
Settings setup, using the supported model and no terminal preparation. The
same node must serve the movie and produce the captions, with measured useful
startup/runway or candid bounded lag. Declaring every common route unavailable
and requiring another worker does not satisfy this request. Encoded playback
may remain unsupported only with a specific measured resource/quality reason
and an explicit status; that does not waive the ordinary single-node floor.
If P0 finds no such profile, the goal remains incomplete and the design/model/
admission work must continue before claiming completion.

### 1.1 The initial scope has a narrow output contract

Initial output is English text translated directly from foreign speech.
Source language is explicit and must be supported by the verified
multilingual model. Tagged audio still requires the correct ordinal;
untagged audio requires a language choice. English-only models are ineligible
for foreign-input translation, even if they are installed and verified for
transcription. Settings must explain this and offer the suitable model by
name. Missing readiness remains advisory to the saved enable choice.

Live microphone/TV feeds, speech synthesis, dubbing, arbitrary target
languages, speaker identification and translation of existing caption files
are outside this first build. Their timelines, permissions or model contracts
differ. Do not infer their availability from this feature.

### 1.2 Optional activation and permissions stay explicit

Keep translation disabled by default until the administrator enables it.
Place its setup in Settings → Developer while qualification remains open,
with the exact graduation evidence listed. Install/verify remain admin-only.
The first implementation retains admin-only work creation **and demand
mutation**, matching current generation. Only an admin playback can create,
refresh, seek-update or release a work-producing demand. Read-authorized
nonadmins may fetch already-published manifests/segments and choose a track;
their polling, playback, seeking or read subscriptions cannot enqueue or
prioritize translation. Seeking into missing coverage reports **Not translated
here yet** and directs the viewer to an administrator. Reading never creates
a hidden demand. This limits the initial nonadmin experience deliberately.
Expanding viewer work creation requires explicit bounded demand authorization,
quotas and a reviewed change to this contract; it is not implicit consumption.

Activation is separate from choosing a track for one playback. Never turn
translation on because a model became ready, or select translated captions
because another viewer requested them. Preserve saved controls during model
status polling, CAS conflicts and partial per-worker installation failure.

## 2. Measure the engine before freezing production window sizes

The current qualified adapter is the CPU-only, two-thread, one-processor
whisper.cpp contract. Its English JFK sample is about 11 seconds. Packaging
reported native ARM64 base/base.en inference times of 1.929/2.070 seconds and
Linux ARM64 times of 2.312/2.442 seconds. These are bounded English
transcription samples, not foreign translation, movie-length throughput or
startup/seek evidence. Reverify the final engine pin and receipts when the
current effort closes; this draft does not grant them broader qualification.

The multilingual base catalog has 99 spoken-input languages and excludes
`yue`; `base.en` cannot handle foreign audio. Input coverage is not an English
translation quality guarantee. The current adapter launches per-file child
processes; it does not offer a reusable rolling recognizer state.

### 2.1 The experiment records latency, cost and correctness separately

Use rights-clear speech in at least three supported source languages,
including one with a different writing system. Include silence, music,
multiple speakers, accented speech, fast dialogue, sentence boundaries,
forced captions and a film-length fixture. Record exact source hashes,
model/engine/package hashes, native platform, CPU/threads, memory, power mode,
foreground delivery type and concurrency. Do not extrapolate ARM64 to x86.

| Measurement | Definition and interpretation |
|---|---|
| Realtime factor (RTF) | Processing wall seconds / covered source seconds. Report decode, inference, validation and total separately; RTF below 1 on one window is insufficient for sustained ahead-of-playback delivery. |
| Startup | User request to first committed translated cue and first useful covered range. Include queue/install separately; never hide a download in an inference number. |
| Runway | Committed contiguous coverage end minus current source playback time. Negative runway means behind; disjoint future ranges do not count as contiguous runway. |
| Lag | Source playback time minus end of the latest usable covered interval containing or preceding that time, with uncovered gaps identified separately. |
| Seek recovery | Seek acceptance to first correct translated cue at the new source time; include both cached-range and missing-range cases. |
| Resource interference | Foreground start/stall/seek metrics with and without translation, child CPU, RSS and held I/O. Faster captions do not excuse degraded playback. |
| Quality | Human bilingual review for omissions, hallucinations, negation, names, dialogue order and timing; compare full-context and chunked translation. WER alone is not a translation-quality score. |

Experiment with bounded source windows such as 10, 20 and 30 seconds,
left/right context overlap such as 2 and 5 seconds, and bounded context
strategies supported by the pinned engine. Those are experiment inputs,
not promised latency, production defaults or quality thresholds. Retain
per-window distributions, p50/p95 and worst observed gaps. Before milestone
P2, write the chosen production limits and acceptance thresholds into this
plan with the measured tradeoff. If none meets the approved useful-lag and
quality budget, do not market realtime; revise the model/adapter or product
status explicitly. Progressive availability may still be useful while slow.

## 3. One immutable translation asset, many playback demands

Reuse current verified model installation and durable job ownership where
they preserve correctness. Do not add a second queue hidden in the player.
The current full-file transcription payload and completed-caption publication
are not sufficient for partially available, seekable assets.

```text
 explicit audio/language + verified multilingual profile
                         │
                         ▼
 immutable asset identity ── viewer demands with independent generations
                         │              │ seek/cancel/expiry
                         ▼              ▼
 durable bounded range claims → foreground-safe worker admission
                         │
 decode source window → translate → validate and reconcile overlap
                         │
                         ▼
 immutable cue segments + CAS manifest/coverage revision
                         │
              authorized revision-aware clients
                         │
           original / Off / translated track selection
```

### 3.1 Identity includes every semantic input

Version a new translation pipeline contract. The asset key includes exact
file/source revision, audio ordinal, normalized spoken language, output
`en`, task `translate`, model digest, engine/adapter contract, decoding
parameters, time origin, overlap/context policy and pipeline version.
Translation must not share transcription receipts merely because file,
audio and model are the same. A model or source change creates a new asset.

The globally portable content revision is a cryptographic digest of source
bytes from a trustworthy immutable/snapshot source, or a verified content
fingerprint protocol frozen in P1. Scanner `size`/`mtime`, a DB file ID and a
local inode are **not** byte equivalence. They remain local change fences but
cannot allow two workers with different equal-size/equal-mtime files to
contribute ranges to one asset. Same bytes on different inodes may share an
asset only after the portable fingerprint matches.

The conservative initial candidate is SHA-256 of the complete opened source,
verified against the authoritative source revision before that worker may
claim ranges, with held-file identity/change checks throughout decoding.
Fingerprint caching must bind the actual verified file identity, not just the
scanner row. Define mutation/snapshot guarantees explicitly; if the source can
change invisibly after verification, use a verified immutable snapshot or
refuse that source. Do not substitute a few matching samples and call them a
content digest. A local cache-copy path alone proves neither provenance nor
cross-node byte identity.

This baseline may cost a full source read before first translation. P0 must
measure cold large-file fingerprint latency and I/O under foreground playback,
not hide it behind warm receipts. If it violates the approved useful startup
budget, P1 must qualify a trustworthy precomputed fingerprint or verified
content-addressed transfer/range protocol before broad support. Until then,
show **Checking source** or restrict supported topology/source profiles;
do not silently weaken equivalence to make startup numbers look better.
Core/lead own the identity protocol and engine owns held-source enforcement.
Test equal scanner metadata with different bytes on two nodes (refuse), same
bytes/different inodes (allow after verification), replacement during hashing,
replacement during a range, cache invalidation and cold/warm source checks.

A worker claims a bounded range with a durable claim fence, canonical range
identity and observed asset revision. It can publish only for that lease,
source and profile. Validate the opened source before and after reading;
source replacement, expired claims and old seek-demand observations cannot
publish to a newer source/profile. Use current exact revision strings on the
wire; do not round IDs, sizes or timestamps through JavaScript numbers.

### 3.2 Partial publication must never imply a completed film

Store immutable validated cue segments with absolute source timestamps and
content digests. Publish segment references and normalized coverage intervals
through one atomic manifest CAS. A partial manifest has a monotonic revision,
explicit state, committed ranges, gaps and any contiguous watermark. An
out-of-order range is legal; a false continuous prefix is not.

Each segment owns one canonical half-open source interval
`[k × W, min((k + 1) × W, duration))`, including when requested by an isolated
seek. Decode that interval plus independently acquired left/right context:
`[max(0, start − L), min(duration, end + R))`. `W`, `L`, `R` and the cue
reconciliation algorithm are immutable profile inputs selected in P0. A
segment cannot depend on a next segment claim, another viewer, or a later
model response to become final. The worker reads its own right context before
committing; it never publishes speculative tail text and waits for a future
claim to make it correct.

At source beginning, missing left context is explicitly empty; at true EOF,
right context ends at verified source duration and the last short core is
finalizable immediately. For a disjoint demand, decode the same canonical
core/context as a sequential request. On cancellation, commit only a fully
validated core for a still-current lease or publish nothing from that attempt;
do not hold an otherwise completed core hostage to the next request. Silence
can be a validated covered core without fabricated cues. Coverage describes
only owned cores, never the context read around them.

The initial candidate policy reconciles overlap within that segment's own
context result, discards context-only cues and clips retained cue timing to
its owned core. No cue may extend its artifact's ownership beyond the core.
Never mutate cues already committed. This establishes independent temporal
ownership but does **not** prove semantic seam quality: clipped dialogue may
repeat or lose words if independently inferred windows disagree. P0 must
compare this policy against full-context truth at every seam and either
freeze a qualified deterministic text/timestamp reconciliation rule or reject
it and revise this section before code. Do not call midpoint/digest matching
a proof of linguistic deduplication. Validate finite timestamps, source
bounds, cue/byte limits and UTF-8; failed seam validation cannot advance
coverage. P2/P3 own regressions for isolated middle core, beginning, true EOF,
short last core, disjoint ranges and cancellation before/after finalization.

Mark complete only when validated coverage spans the full required source
range and all segments belong to the same source/profile. Existing downloaded
caption metadata continues to describe complete captions. Partial manifests
need explicit provenance/state, not an ordinary complete WebVTT track with
missing tail silently treated as EOF. Interrupted jobs retain only committed
segments; private scratch never becomes selectable content.

### 3.3 Publication includes durable bytes, not a worker-local pathname

Proposed initial storage is a new bounded immutable text-segment record in
the existing authoritative replicated store, keyed by asset/segment digest.
Store the actual platform, engine binary SHA-256, model/profile/source digest,
operation epoch and producing claim fence on each segment. A common adapter
contract does not promise bit-identical inference across platform builds.
The first valid immutable CAS winner defines the accepted bytes/digest; a
second worker cannot replace that segment with a different inference result.
Cross-platform seam/quality qualification remains per claimed profile.
Store the validated cue bytes, byte length and digest with provenance before
or atomically with the manifest reference. Only an applied durable record may
be advertised. A worker's local cache or successful upload request alone is
not publication. This deliberately pays replicated text-storage cost to avoid
inventing an unqualified cross-node blob availability protocol.

All serving nodes use the normal authorized API and authoritative/read-fenced
store route to fetch the durable segment. Verify its digest before serving;
a missing/corrupt segment reports unavailable/retry rather than successful
empty captions or fabricated coverage. A node unable to observe the manifest's
commit revision must route/read from authority or refuse; it cannot return a
new manifest and stale local missing bytes as a successful pair. Cluster
failover follows replicated-store durability. A single-node install has no
redundancy claim when its only durable store is unavailable.

Choose segment/transaction byte limits against the real store command budget
in P1. Large output is split into bounded immutable records before commit,
never an oversized replicated command. Retain unreferenced staged records
for a bounded orphan grace period, then GC only after checking no retained
manifest revision or active read lease references them. Manifest retirement
and reader-lease acquisition use a serializable boundary so GC cannot delete
bytes between a valid manifest read and fetch. Revocation ends permission,
not the internal storage read lease; the lease must not bypass authorization.
Crash between byte insert and CAS leaves an orphan, not an advertised missing
segment. Crash after CAS leaves durable fetchable bytes.

Core and lead must prove publish/failover/fetch, stale replica routing,
corruption refusal, failed CAS orphan collection and GC/read races with
genuine SQLite and Hiqlite tests. If replicated cue storage is rejected for
measured scale, replace it with an independently qualified durability and
placement protocol before P2; do not fall back to worker-local paths.

### 3.4 Viewers request ranges; they do not own the shared asset

Each work-producing demand requires admin authorization and identifies
authenticated admin/playback, source position,
playback rate, generation, bounded expiry and required window. Coalesce
identical compatible ranges across viewers. Retain independent demands when
viewers watch different positions. Prioritize the earliest useful missing
range near an active viewer, with bounded fairness across viewers.

Seeking updates that viewer's demand generation. Cached ranges can be reused;
missing ranges are requested around the new absolute source time. Ignore old
responses for that viewer, but do not invalidate still-useful published ranges
or another viewer's work. No stale translated cue from the pre-seek position
may remain visible at the new position. Playback seeking and audio control
continue to use their existing owners; translation never implements a second
seek or restarts the video.

Stopping one admin demand cannot cancel another admin viewer's claim. Stop unused work
when all demands expire, subject to reviewed bounded finishing policy. An
administrator's Stop cancels the durable operation and prevents late publish;
committed segments remain usable unless explicitly removed. Quotas bound
range churn, active demands, per-viewer requests, retained bytes and retries.
Pause suspends unneeded lookahead after bounded expiry; resume refreshes demand.

### 3.5 Administrative Stop latches the operation, not just its children

The immutable content/profile asset is separate from a mutable durable
operation epoch. Every work-producing demand, range claim and publish attempt
carries that operation epoch plus its own demand generation/claim fence.
Administrative Stop atomically advances the operation epoch, records a Stop
latch/tombstone and invalidates scheduling rights for all earlier demands.
Cancel in-flight range claims and reject their late publications through the
same epoch boundary. Committed segments remain authorized/readable; they do
not imply that unfinished ranges may continue.

A heartbeat or seek from an already-active demand cannot clear the latch,
even if its per-viewer generation increases. Replayed pre-Stop creation or
Retry requests are idempotent stale intents, not new permission. Resumption
requires an explicit new administrator intent/Retry, with fresh request ID,
observed stopped epoch and CAS; it creates a new operation epoch and fresh
demands. No browser refresh or subscription automatically performs this action.
Freeze replay retention and expected-epoch errors in P1.

Lead/core own transactional Stop/claim/publish semantics; web/native own
explicit resume intent. Genuine multi-viewer tests keep two admin heartbeats
and seek updates running after Stop and assert zero new claims, zero late
publication and readable committed segments. Also test Stop racing claim,
Stop racing manifest CAS, repeated Stop, old intent replay, process restart
and one explicit valid Retry. Releasing one viewer's demand is not global
administrative Stop and does not advance the shared operation epoch.

### 3.6 Source time mapping is a decoder contract, not a seek argument

Canonical cue time is the same absolute source timeline the playback decision
uses, before user subtitle offset or playback-rate presentation. The profile
records the timeline contract version. Probe stream time bases, container
origin/edit lists, audio start/delay and first valid sample PTS. Preserve
rational time until final bounded millisecond rounding. Negative PTS is
normalized once against the common playback origin; do not independently
zero every chunk or audio stream.

An FFmpeg `-ss` request is only a decode hint. The adapter must observe the
actual first retained decoded audio sample timestamp and discard to the
requested core/context boundary with sample accounting. Record the exact
anchor and sample rate after delay compensation/resampling. Convert engine
relative time by `source_seconds = first_retained_sample_source_seconds +
relative_sample_index / sample_rate`; account for model timestamp units and
resampler delay explicitly. If this cannot be established for a container,
refuse that profile instead of guessing the requested seek time was exact.

VFR video does not redefine the audio clock. Audio stream offsets, negative
starts, nonzero container starts, codec skip samples and edit-list delays
must align to the existing player's source clock. A viewer subtitle offset
is applied by the client after canonical mapping, never baked into shared
segment identity or applied twice. Playback rate changes scheduling/runway,
not source timestamps. An audio switch changes asset identity and retires the
old playback demand while preserving already-published old assets for their
other viewers. Engine/core/native owners freeze the mapping fixture oracle
in P1: a known click/spoken marker with matching video/source timestamps,
including nonzero/negative PTS, VFR and delayed audio. Test seeks before,
inside and after each offset and check the rendered cue against the same
source clock, not just JSON timestamp equality.

## 4. Translation yields to the picture

Use the durable resource/admission layer, including exact worker membership,
source accessibility, verified multilingual readiness and current capacity.
Fresh signed observations distinguish unknown worker state from missing
models and actual playback pressure. Avoid inferring readiness from bytes or
from the small English self-test.

The current `fragment_worker_idle` predicate requires zero active admissions.
Calling it unchanged while this same node serves encoded playback would yield
every translation range forever. The new feature must not claim progressive
same-node support through a policy that categorically denies its work.
P0 measures the entire source-check → admission → decode → inference →
publication path while actual direct, remux and encoded playback is active.
Report single-node and two-node results separately, including the case where
only the busy foreground node can read the exact source.

P1 chooses an atomic admission contract with explicit translation CPU/memory/
I/O and decode credit only if measured spare capacity preserves the approved
foreground budgets. Otherwise restrict the qualified profile/topology, for
example to another idle compatible source-equivalent worker, and report the
restriction in Settings/player status. Single-node encoded playback cannot be
marked supported merely because a model benchmark was faster than realtime.
No eligible spare worker means truthful waiting/unavailable status; no
unreviewed bypass of current admission, and no starvation disguised as normal
transcribing. Lead owns this contract; engine provides measured non-preemptible
child bounds. Regression controls cover one-node direct/remux/encoded,
two-node source eligibility, foreground admission races, yield/recovery and
multiple translation demands. Do not promote a profile without its end-to-end
concurrent admission result.

Foreground playback has priority. Bound decode/inference chunk duration,
CPU threads, memory, concurrency and pending ranges. A non-preemptible child
must have a measured worst-case time and a cancellation path; if it cannot
yield safely, refuse that profile while the foreground workload is active.
Audio-only decoding is not zero-cost and must acquire the relevant resources.
Do not raise decode concurrency merely to recover caption lag. Test forced
foreground start, seek, failover and source loss during each processing phase.

Restart may abandon private work and restart a range. The durable range claim
and manifest CAS prevent duplicate publication. Report queued/admission wait,
downloading, verifying, translating, validating, available partial, catching
up, complete, failed and cancelled as distinct safe states. A partial track
can remain usable after a range failure; the status must show the gap and a
bounded Retry action instead of declaring the whole film ready.

## 5. Proposed API and client delivery contract must be frozen before code

This section is a proposal, not an implemented API. Reverify current routes
in [the usability contract](SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md#9-frozen-build-contract)
and existing store, native subtitle and playback-owner code at build time.
Reuse managed model catalog/install/verify; introduce separate versioned
translation settings with CAS rather than silently changing same-language
transcription settings or legacy Advanced mode semantics.

| Proposed route | Contract to freeze in P1 |
|---|---|
| `GET /api/v1/files/{id}/subtitle-translation/options` | One revision-bound read: exact source strings, raw/normalized audio languages, input-compatible models, output `en`, original tracks and partial/complete translation provenance. No paths, tokens, VTT bodies or raw process errors. |
| `POST /api/v1/files/{id}/subtitle-translation` | Versioned exact source/audio/input/output/model/profile request. Returns exact durable job and asset IDs; refuses unsupported language/model, source change, permission or quota explicitly. |
| `PUT /api/v1/subtitle-translation/assets/{asset}/demands/{demand}` | Authenticated playback-bound bounded source position/rate/window, monotonic viewer generation, CAS or idempotency semantics and expiry. Asset authorization rechecked for every mutation. |
| `DELETE .../demands/{demand}` | Idempotent viewer demand release. Never a shared-asset cancellation; administrative Stop is separate. |
| `GET .../assets/{asset}/manifest` | Bounded safe revision/state/coverage/segment references, failure reasons and retry timing. ETag/revision conditional reads; no false cache permanence for evolving manifests. |
| `GET .../assets/{asset}/segments/{digest}` | Authorized immutable text segment, explicit media type and cache identity; access revocation still enforced. A digest is not an authorization token. |

Freeze exact JSON DTOs, error vocabulary, persistence schema, cache lifetime,
size bounds, source timebase and idempotency in P1. Do not copy these route
shapes into implementation before review. Distinguish publication revision
from worker fence and from viewer seek generation; none substitutes for the
other two.

### 5.1 Growing delivery cannot depend on refetching a native track URL

Web can fetch immutable segments and update a script-owned text track while
retaining one video/player/attachment. Deduplicate by cue identity and prune
bounded old cues. Selection changes and stale asynchronous responses must be
fenced by account, page, file, audio, profile and playback generation. No new
source URL, video replacement, automatic Off override or hidden burn-in.

Apple and Android require explicit delivery adapters; a native player may
cache a static WebVTT URL or never reload its tail. Evaluate script/overlay
cue adapters or correctly versioned sidecar/HLS subtitle delivery against
existing subtitle owners. Keep source-time mapping, offset, rate, AirPlay,
PiP, fullscreen and accessibility on each client's established path. Do not
claim cross-client support because web script cues work. Unsupported client
routes must report capability before the user selects the partial track.

### 5.2 Manifests are bounded snapshots; clients can always resynchronize

P1 freezes maximum segment bytes/cues, manifest page bytes/entries, coverage
interval page size, fetch concurrency, retained revision count/age and
client cue-memory windows against the measured/store budgets. Reject limits
that merely move unbounded full-film state into one JSON array. Both segment
references and normalized coverage intervals are paginated. An opaque cursor
binds asset identity, immutable snapshot revision and last ordered key; pages
from different revisions cannot be combined. The current summary is bounded
and names the snapshot revision/ETag and whether more pages exist.

A client pins a retained snapshot/read lease while paging the ranges needed
for its playback window. New publication may create a newer snapshot without
changing existing page contents. Conditional reads returning 304 preserve the
known snapshot only; they do not imply no gaps or job success. On an ETag or
revision gap, expired cursor/lease, app restart or unknown delta base, discard
the incomplete index and request a new authoritative snapshot for the active
source window. Duplicate/reordered pages must not duplicate cues. Bound retry
backoff and expose unavailable status while resynchronizing.

Clients prune displayed cues and downloaded bytes outside a bounded source
window, retaining only bounded revision/coverage metadata. A backward seek
re-fetches pruned segments from the durable authorized route and reconstructs
cues with the same stable IDs; it never treats pruning as server deletion or
reruns already-published inference. Missing or corrupt referenced bytes trigger
safe resync/refusal, not a false completed range. Web/native owners prove
multi-page snapshots, concurrent publication during paging, revision gaps,
restart, cursor expiry, pruning/backward seek and repeated cue delivery.

### 5.3 The UI explains partial coverage without stealing selection

Settings names the compatible multilingual model, download size, worker and
fresh verification state. Installation includes a translation-specific
foreign-input→English self-test before translation readiness is reported;
existing English transcription verification remains a distinct capability.

Player controls label the track **English · AI translation · building** and
report preparing/available/behind/gap/failure separately from selection. Let
the requester explicitly choose **Use translated captions**; never select it
on behalf of another viewer or after the viewer chose Off/original captions.
Preserve the original caption menu entries throughout updates. A seek into
an uncovered region clears obsolete cues and says **Translating this part**;
it never replays previous-position text or pauses the picture.

Announce state transitions and actionable failures through a polite status
region; do not announce every inference token, cue or second of lag. Preserve
keyboard focus across status/track refresh and fullscreen transitions. Label
language, audio ordinal, model, partial quality and Retry controls. Render
errors and translated text safely as text, never trusted HTML. Translation
captions are an estimate and may be wrong; keep that note visible in setup
and track information without obstructing playback.

## 6. Serial milestones and exact ownership keep parallel work reviewable

Start only after current subtitle usability is complete. Create
`effort/progressive-subtitle-translation` from then-current main; refresh all
paths/signatures below first. Task branches target the current effort. Sol
agents implement; the parent coordinates and adversarially reviews. Use the
repository normal hooks, focused regressions and manually dispatched effort
gates; promotion requires exact-candidate qualification and main promotion.
No deployment or physical build is implied by this plan.

| Milestone | Exclusive owner and files | Acceptance before the next milestone |
|---|---|---|
| P0 · characterize | Engine agent: new experiment driver/fixtures under `tests/fixtures/subtitle-translation/` and qualification receipt under `docs/features/`; plan agent owns this plan/index/backlog. No production code. | Reproducible source/package/model hashes, RTF/startup/seek/overlap/quality/resource matrix; selected production bounds and candid supported profile list. |
| P1 · freeze contracts | Lead: exact new core schema/API/profile/timebase contract; parent review then independent engine/UI review. | Golden DTOs and explicit permission/idempotency/CAS rules, state machine and counterexamples agreed in this document. No implementation starts with an unresolved semantic choice. |
| P2 · durable asset/ranges | Core agent: new `crates/plurx-core/src/store/subtitle_translation*.rs`, shared job enums/traits/registries and SQLite/Hiqlite wiring/tests. Lead alone integrates shared files. | Genuine SQLite + Hiqlite tests for multi-viewer overlap, Stop/fence/lease/source replacement, out-of-order coverage CAS, restart and quotas. |
| P3 · engine capability/runtime | Engine agent: new daemon `subtitle_translation/runtime.rs`, translation self-test/profile helpers and engine/model packaging files. Existing `subtitle_transcription.rs` changes only through lead-approved seam. | Foreign→English translation and stable overlap on selected profiles; cancellation/admission/source/failure corpus with no private artifact publication. |
| P4 · manager/API/admission | Lead: new daemon `subtitle_translation/mod.rs`, `api.rs`, settings, worker/state/route integration and foreground admission. Own shared files from P2 onward. | Auth/revocation, dedup/independent demand lifetimes, fairness, exact manifest publication and safe statuses through real daemon routes. |
| P5 · web | UI agent: Settings Developer, detail/preplay, watch browser, watch presentation/projection, subtitle track owner and targeted web/browser tests. Declare exact existing filenames in P1 after current effort lands. | Real installed multilingual model; foreign speech+original captions; first partial cues during attached playback; lag/gap/seek/original/Off/Retry/keyboard/fullscreen; no playback reopen. |
| P6 · native delivery | Apple agent owns native subtitle adapter/UI/tests; Android agent owns Android subtitle adapter/UI/tests. Freeze exact file lists from then-current tree in P1. | Native compile/unit + emulator/simulator controls; separately retain required physical display/PiP/AirPlay receipts before claiming those routes. Unsupported routes remain explicit. |
| P7 · qualification/promotion | Lead and reviewer: exact candidate, Operations/API/Features updates, canonical receipt, this ledger/index/backlog. | Full matrix below, independent code review, current-main integration, main gate and explicit qualified capability boundaries. |

Existing shared files have one integration owner. Do not concurrently edit
job registries, routes or player track owners from separate task branches.
If the P1 inventory reveals ownership overlap, split leaf modules or sequence
the work; do not rely on resolving behavioral conflicts after parallel edits.

### 6.1 File reservations are exclusive even when tasks overlap

These are concrete reservations against the draft tree. Proposed new files
are marked **new**; their absence is intentional. P1 must reconcile a moved
path before assigning work, and must list any additional shared file before
editing it. This table gives ownership, not permission to start early.

| Owner | Reserved files |
|---|---|
| Plan/review agent | `docs/features/PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md`, `docs/README.md`, `docs/features/PROJECT-BACKLOG.md`; this agent alone updates the planning ledger during execution. |
| Core agent, P2 | **New:** `crates/plurx-core/src/store/subtitle_translation.rs`, `subtitle_translation.sql`, `subtitle_translation_tests.rs`, `hiqlite_subtitle_translation.rs`. Lead integrates their trait/schema/registry includes after focused review. |
| Lead, all shared core integration | `crates/plurx-core/src/store/mod.rs`, `background_jobs.rs`, `background_jobs_schema.sql`, `background_jobs_transcription.rs`, `hiqlite.rs`. Any extra registry discovered in P1 belongs to lead, not parallel leaf agents. |
| Engine agent, P3 | **New:** `crates/plurxd/src/subtitle_translation/runtime.rs`, `profile.rs`, `self_test.rs`; experiment `tests/fixtures/subtitle-translation/measure.py` and rights/source manifest. Engine pin/catalog/package changes require their exact packaging file list to be frozen from the completed usability effort before P3. |
| Lead, P4 | **New:** `crates/plurxd/src/subtitle_translation/mod.rs`, `api.rs`, `settings.rs`; existing `crates/plurxd/src/main.rs`, `state.rs`, `http/mod.rs`, `subtitle_models/mod.rs`, `subtitle_transcription.rs` and worker/admission shared modules identified in P1. Lead alone registers all new modules/routes. |
| Web agent, P5 | `crates/plurxd/src/web/pages/settings-developer.js`, `detail/preplay-selection.js`, `detail/watch-browser.js`, `player/watch-presentation.js`, `player/projection-chrome.js`, `player/audio-sync.js`, `app.css`; **new:** `player/subtitle-translation.js`. Lead approves the small asset-registration edits to `web/index.html`, `http/mod.rs`, `docs/clients/WEB-SHELL-LAYOUT.md` and `crates/plurxd/src/web/jsconfig.json` together; no imports/modules in plain global scripts. |
| Web agent, focused regressions | `tests/web/subtitle-generation.test.js`, **new:** `tests/web/subtitle-translation.test.js`, `tests/web/subtitle-translation.browser.cjs`; `tests/playback/web-control.test.js` only for declared existing-owner interactions. Lead owns Makefile/CI inventory registration. |
| Apple agent, P6A | `clients/apple/Sources/PlayerController.swift`, `PlayerView.swift`, `PlayerItemObserver.swift`, `PlurxAPI.swift`, `SettingsStore.swift`, `SettingsView.swift`; **new:** `ProgressiveSubtitleTranslation.swift`, with focused tests under `clients/apple/Tests/`. No simultaneous other Apple agent edits to these files. |
| Android agent, P6B | `clients/android/app/src/main/java/tv/plurx/app/player/SharedPlaybackOwner.kt`, `PlayerScreen.kt`, `SubtitlePolicy.kt`; `data/PlurxApi.kt`, `data/SettingsStore.kt`, `ui/SettingsScreen.kt` under the same app source root; **new:** `player/ProgressiveSubtitleTranslation.kt` and matching focused unit/instrumented tests. |
| Lead/reviewer, P7 | `docs/API.md`, `docs/FEATURES.md`, `docs/OPERATIONS.md`; **new:** `docs/features/PROGRESSIVE-SUBTITLE-TRANSLATION-EVIDENCE.md` plus indexed receipts. Plan agent applies reviewed state changes to its reserved ledger/index/backlog files. |

Do not reserve a new flag in Settings merely for internal correctness. The
optional translation enable choice has a user purpose; internal partial
publication, source fences and admission remain unconditional correctness.

## 7. Acceptance proves progressively useful captions on the exact candidate

| Case | Required observation and retained evidence |
|---|---|
| Clean setup | Empty model store, packaged engine, cleared inherited model/engine environment. Install and translation-specific verify through named Settings controls; no terminal model work. Preserve saved enable even when unready. |
| Single-node acceptance floor | At least one ordinary packaged single-node movie route serves video and useful progressive English captions after Settings installation. Record named route/profile and useful measured startup/lag. No success claim based solely on another worker or all-common-routes refusal; encoded exclusions require specific evidence. |
| Progressive proof | Foreign speech longer than the first processing window, with original captions. At least one correct translated cue is visible while later ranges remain unprocessed. Record manifest revisions, coverage, source position, video/player/attachment identity and screenshots/video. A completed-file track does not pass. |
| Correct timing/quality | Bilingual human samples including overlaps, names, negation, fast speech and silence. Absolute source times, stable boundaries, no doubled/dropped overlap, no hallucinated silence cues. Retain annotated disagreements and accepted limits. |
| Slow/unknown capacity | Deliberately slower-than-playback worker, stale observations and no worker. Show actual behind/gap/unknown status, not false playback pressure or realtime. Picture continues and original/Off controls work. |
| Seek/rate | Forward/backward seeks into covered/uncovered ranges, repeated seeks, pause/resume and playback-rate changes. No pre-seek stale cue; bounded demand churn and measured recovery; no video restart. |
| Multiple viewers | Same range coalesces; different ranges remain fair; one viewer Stop/logout/disconnect does not cancel another. Admin cancellation prevents late publication. |
| Faults | Network/model integrity/disk/reservation/engine failure, decode/inference timeout, invalid/oversized output, worker crash/restart, stale lease, source replacement and loss. No unsafe partial or false complete result; Retry does not duplicate committed cues. |
| Delivery | Real web and each claimed native client, fullscreen, keyboard/screen reader, subtitle offset/rate, PiP/AirPlay where claimed; revocation and evolving-cache behavior. Original track choice/Off never overridden. |
| Playback protection | Supported direct/remux/encoded foreground workloads with translation; retain start/seek/stall and resource comparisons against no-translation controls. Enforce chosen P0 budgets. |

Synthetic unit/browser API fixtures prove software boundaries only. They
cannot qualify translation quality, throughput, package installation or real
partial delivery. Keep unit, browser-fixture, real-daemon and physical-client
receipts separate. Never convert an unrun device case into a passed checkbox.

## 8. Decisions still to freeze, and review ledger

| Decision | Current proposal / missing evidence | Owner |
|---|---|---|
| Useful lag and startup budget | User chose progressive, not a latency guarantee. P0 must propose numeric profile-specific thresholds from measurements for parent approval. | Engine + parent |
| Model/engine selection | Try existing multilingual base first; no assumption of sufficient quality or sustained RTF. A different model requires catalog, license, package and self-test review. | Engine |
| Stable overlap/context policy | Commit only stable interiors with deterministic ownership; compare quality against whole-context baseline before choosing exact windows. | Engine + core |
| Demand finishing/retention | Bound work after last viewer leaves, per-user requests, retained bytes and TTL. Set numbers from measured storage/admission budgets in P1. | Lead |
| Native growing-track adapter | Static VTT URL reload is not assumed. Select existing-owner-compatible text/overlay/HLS routes per client after a minimal measured proof. | Native owners |
| Viewer work-creation permissions | Initial admin-only demand creation/refresh/seek/release. Nonadmin reads cannot schedule; missing ranges stay unavailable until an admin demand produces them. Broader viewer requests require a reviewed permission/quota change. | Parent + lead |
| Current feature dependency | No translation implementation before subtitle usability is complete; rebase plan/contracts to the final qualified current tree. | Lead |

| Review | State as of 2026-10-15 | Disposition |
|---|---|---|
| Parent adversarial design review | Completed 2026-10-15; revised R1–R9 and single-node acceptance floor approved in design | No implementation or measurement approval implied; P0/P1 decisions remain open. |
| Engine independent second review | Completed full-plan review 2026-10-15; R6–R8 and provenance correction recorded below | Parent and engine confirmed revised dispositions in design on 2026-10-15; P0/P1 measurement/protocol decisions remain blocking. No implementation approval implied. |
| Implementation authorization | Deferred until current feature completes | Plan-only branch contains no translation implementation. |

### 8.1 Parent adversarial findings and proposed dispositions

Parent reviewed and approved the R1–R5 architectural dispositions on
2026-10-15. They are resolved in design, not implemented or qualified.
P0/P1 numeric bounds and semantic-seam decisions remain explicit prerequisites.

| Finding, 2026-10-15 | Revised contract / exact regression obligation | Owner and status |
|---|---|---|
| R1 · viewer consumption could implicitly create work | §1.2/§3.4: all work-producing demand mutations are admin-only; read-only viewer seek/poll cannot enqueue. Test nonadmin POST/PUT/DELETE refusal and unchanged job/priority counts after seeks into missing coverage; authorized existing segment reads still pass. | Lead; resolved in design by parent; implementation/regression outstanding. |
| R2 · a stable interior might wait forever for the next claim | §3.2: canonical independently finalizable core with its own bounded context; explicit beginning/EOF/disjoint/isolated-seek/cancel rules. Test final core with no successor, isolated middle core and cancellation at every publication boundary. Semantic seam policy remains a P0 quality decision, not an unproved dedup claim. | Engine + core; resolved architecture by parent; P0 seam decision/measurement outstanding. |
| R3 · manifest CAS could publish unreachable worker-local bytes | §3.3: bounded immutable cue bytes in authoritative replicated store before/with references; read-fenced routing, digest validation, orphan grace and GC/read leases. Genuine Hiqlite failover/read/GC race tests required. | Core + lead; resolved architecture by parent; P1 store-budget freeze/implementation outstanding. |
| R4 · chunk offset did not define playback time | §3.6: actual decoded-sample anchor and rational stream/container mapping, delay/resampler accounting, negative/nonzero PTS, VFR and client-only subtitle offset. Test against visible/audio source markers after seeks and rate/offset changes. | Engine + native/core; resolved architecture by parent; P1 mapping oracle/implementation outstanding. |
| R5 · growing arrays and lost revisions could break resync | §5.2: bounded immutable revision pages/cursors/read leases, ETag-gap/restart reset, pruning and backward re-fetch. Test concurrent updates while paging, expired cursors, duplicate pages and pruned backward seek without new inference. | Lead + web/native; resolved architecture by parent; P1 numeric limits/implementation outstanding. |

### 8.2 Independent engine findings and proposed dispositions

The engine agent reviewed the complete `d78bfaf4` draft independently after
parent R1–R5 review. Parent approved these revised architectural dispositions on 2026-10-15.
The engine independently confirmed closure against revision `a0c3da8f` on
2026-10-15; neither review qualifies performance or implementation. The single-node acceptance floor in
§1 is mandatory and cannot be waived by unsupported-profile declarations.

| Finding, 2026-10-15 | Revised contract / exact regression obligation | Owner and status |
|---|---|---|
| R6 · active demand heartbeats could recreate cancelled work after Stop | §3.5: durable operation epoch and Stop latch fence demand scheduling, claims and publication. Only explicit fresh admin intent with observed epoch can resume. Test two continuing heartbeats/seeks after Stop, zero new claims/late publish, readable committed segments, crash/replay and explicit Retry. | Lead/core + clients; resolved in design by parent and engine; implementation/regression outstanding. |
| R7 · scanner metadata cannot establish cross-node source equivalence | §3.1: trustworthy portable cryptographic content revision; conservative full-source SHA-256/held immutable source candidate, measured cold-read cost and no sampled-hash shortcut. Test two different equal-size/equal-mtime sources refuse, identical bytes/different inode accept after verification, hashing/decoding replacement and cache invalidation. | Lead/core + engine; protocol/usable cold-latency decision must freeze in P0/P1. |
| R8 · current idle predicate would starve during encoded playback | §4: explicitly prohibit treating zero-admission idle policy as concurrent support. Measure actual full path under one-node direct/remux/encoded and two-node cases; freeze spare-capacity admission or declared source/topology restrictions without bypassing foreground protections. | Lead + engine; P0 evidence and P1 admission contract outstanding. |
| R9 · common adapter can hide platform inference differences | §3.3: per-segment actual platform/engine SHA plus source/model/profile/epoch/fence provenance; immutable first valid CAS winner, never overwrite with another platform's result. Test duplicate valid competing results retain the winner and its truthful provenance. | Engine/core; resolved provenance design by parent and engine; implementation/regression outstanding. |
