# Automatic quality — the best sustainable picture on each display

**Status:** combined server/client implementation in progress; combined feature
qualification remains open · Paul authorized GPT-6.1 Sol implementation on
2026-09-30
· **Written:** 2026-09-30 · **Original source baseline:** `ceb7dd8cc`
· **Opus reconciliation source spot-check:** `b8f3c7587`

The original baseline was a feature branch, not main. The checkout moved to
`codex/cluster-dvr-transaction-audit` before this reconciliation; unrelated work
is untouched. Neither this document nor Opus has verified current remote main.
M0 must fetch the authoritative Forgejo main, record its full SHA, recensus the
named seams there and start implementation from it. Do not treat either local
baseline as current-main qualification.

Companion to [adaptive quality](ADAPTIVE-QUALITY.md) (existing web policy),
[playback](../PLAYBACK.md) (delivery paths), and
[quality-switch continuity](../playback-control/QUALITY-SWITCH-CONTINUITY-BUILD.md)
(the replacement protocol). This plan answers how Cinema should preserve
original quality where it plays well, use intermediate resolutions when it
must encode, and recover quality without destabilizing playback. Paul chose one combined feature effort in §6.1. Initial-selection and runtime
Auto work qualify together, incorporating existing A-05 ownership and evidence.
Parser compatibility still rolls out before new protocol use (§4.1). Re-read the named symbols against
the implementation base; the source baseline is a local checkout, not a claim
about what is deployed on the tablet.

## 1. Objective and boundaries

The user's requirement is **the highest useful picture quality that plays
smoothly, without making the viewer manage resolution**. The concrete device
has a 2400×1600 panel. A 16:9 picture fits at 2400×1350; 2560×1440 is a useful
standard transcode size, while a playable original 3840×2160 stream should
remain original. No aspect-ratio change is required.

Success means Auto can select 1440p, explain its choice, step down when a
measured bottleneck warrants it, and regain quality when evidence supports a
safe change. Resolution alone is not a quality score. Re-encoding loss,
bitrate, color fidelity, decoder load, audio preservation and interruption
cost all matter. No implementation can guarantee zero stalls on arbitrary
hardware or networks; §8 defines the measurable acceptance bar.

**In scope:** on-demand video on Android, Apple and web; initial selection,
manual-menu consistency, intermediate SDR/HDR transcodes, runtime Auto,
diagnostics, compatibility and physical-device qualification. Android on the
2400×1600 tablet is the first physical acceptance target, not the only client.

**Non-goals:** Live TV policy changes, offline-download quality changes,
unbounded per-device custom encodes, simultaneous full ABR ladders, a new
playback owner, a new codec family, fleet deployment during planning, and
changing explicit Original or Manual intent. These would expand the work
without fixing the identified choice gap.

## 2. What the current code actually does

| Finding | Source and consequence |
|---|---|
| The shared lower ladder is 144, 240, 360, 480, 720, 1080 | `LADDER_HEIGHTS` in [ladder.rs](../../crates/plurxd/src/transcode/ladder.rs). `advertised_ladder` prepends one source/capability ceiling; a 2160 source normally skips 1440. |
| Intermediate explicit heights already pass through | `snap_height` in that file and `resolve_height` in [create.rs](../../crates/plurxd/src/http/hls/create.rs). Encoder plumbing exists; this is not proof of every intermediate HDR route. |
| 1440 shares the 1080 bitrate today | `bitrate_for_height` assigns 8,000 kb/s from 1080 through 2159. Adding a rung without reviewing its rate model risks a cosmetic resolution increase. |
| Output geometry preserves aspect and avoids upscaling | `output_size` in [core transcode](../../crates/plurx-core/src/transcode/mod.rs) clamps height and rounds dimensions even. Width, sample aspect and rotation still require explicit planning treatment. |
| Android Settings and player menus disagree | [ViewerPreferences.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/ViewerPreferences.kt) contains `Q1440`; [SettingsScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/SettingsScreen.kt) lists the enum; `qualityOptions` in [PlaybackPolicy.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackPolicy.kt) filters server rungs. |
| Android probes decoding, not presentation dimensions | [Caps.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/Caps.kt) probes standard sizes at 30 fps and display HDR. A 2160 decoder ceiling does not mean a 2160 panel or sustained 60 fps. |
| Web already has part of this policy | `playerPixelHeight`, `initialAutoRung` and the adaptive reducer in [playback-policy.js](../../crates/plurxd/src/web/playback-policy.js) use a backing-height cap, throughput, runway, encode margin and cooldowns. Cold start without a prior preserves server Auto. Do not describe web as wholly display-unaware. |
| Server Auto starts from source and route capability | `auto_height_for_request` in [describe.rs](../../crates/plurxd/src/transcode/manager/describe.rs), then `auto_height_from_prior`. Neither receives a fitted presentation rectangle. |
| Legacy server stall stepping is a separate integration point | `one_rung_below` and its caller in [manager/create.rs](../../crates/plurxd/src/transcode/manager/create.rs). Native clients currently repair the same delivery; do not revive legacy quality stepping for an unattributed stall. Any newly attributed recovery must consume the candidate builder. |
| HDR preservation has discrete proofs | `hdr10_rung_fits` admits 1080 software/QSV and 2160 QSV. 1440 is not proved by accepting an integer; codec level, color metadata and output geometry need qualification. |
| Auto and Manual are already distinct | [desired.rs](../../crates/plurx-core/src/playback/desired.rs) has `Auto { height: Option<i64> }`, `Original`, `Manual { height }`. Preserve these meanings and their digest behavior. |
| Control additions require protocol care | `DynamicCapabilities` and `ClientObservation` in [playback_control.rs](../../crates/plurxd/src/playback_control.rs) use `deny_unknown_fields`. New client fields must not be sent blindly to old servers. |

### 2.1 Extend the existing native Auto effort

Paul chose **one combined feature effort** on 2026-09-30. Initial selection,
intermediate/manual choices and runtime adaptation ship as one completed
objective; he explicitly declined Opus's recommendation to release the first
part independently. Apple fresh-transfer evidence and the full native D3
matrix therefore remain blocking for feature completion, with the accepted
integration/dependency cost recorded in §6.1.

Incorporate [A-05's build plan](../clients/NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md)
and [A-04's design](../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md) into that same
execution ledger. Claim/update the existing A-05 board row for runtime work;
do not create a second controller project with its own fixtures or ownership.
At M0, amend A-05 to name this effort and the mapping in §7 before editing its
runtime surfaces. The planning reconciliation did not claim the board or start implementation;
§9 records subsequent source implementation. The labels A (starting selection) and B (runtime)
below are workstreams, not separately shippable feature deliverables.

The strictly additive parser-compatibility prerequisite in §4.1 is a necessary
standalone infrastructure release before new fields appear. It ships no feature
behavior and does not release the starting-selection half early. Plan and land
it as an ordinary prerequisite before the combined behavioral integration
branch, preserving the repository rule that a feature effort promotes only
after complete qualification.

Reuse [auto-quality-policy.json](../../tests/playback/auto-quality-policy.json).
Its `web_current`/`expect` differences are explicit unresolved design findings,
not fixture bugs to normalize away. Before native reducers, settle A-05 M0's
four questions with code and fixture dispositions: stall-scoped holds versus
routine producer pacing, real voluntary cooldown, decode ownership, and
publication refusal versus bandwidth pressure. Do not port current web
behavior blindly. Retain its tested refusal to infer network pressure from
unknown, held, denied-authority or publication-pending states.

A-05 requires a platform's shaped-network evidence **before its controller
PR merges**, using an unmerged branch build; the Developer switch remains
selectable and authoritative. This is an integration requirement, not a hidden
runtime gate. B3/B4 below inherit it; final B5 qualification is not a substitute.
The optional six-switch-per-hour proposal is unresolved, not a shipped
constant. Use the existing rate limit unless trace evidence motivates and
explicitly records a new budget; emergencies cannot exhaust a voluntary budget.

Existing design documents are context, not proof that every milestone shipped.
In particular, the [continuity results](../playback-control/QUALITY-SWITCH-CONTINUITY-RESULTS.md)
have unfilled physical-device rows, and the
[honest master plan](HONEST-MASTER-PLAYLIST.md) records manifest work/evidence
still outstanding. Inventory these dependencies at the relevant M0-A/M0-B instead of silently
assuming completion or reimplementing their owners' work.

## 3. Selection contract

### 3.1 Preserve originals before choosing an encode size

Resolve the complete source, codec/profile, frame rate, dynamic range, audio,
subtitle and device capabilities first. Prefer direct play or a video-copy
remux when compatible, no measured limit disproves smooth delivery, and the
network is viable. A smaller display alone must never force transcoding.
Audio-only conversion must not imply video conversion.

When encoding is required, form candidates from routes the actual worker can
serve and the client can decode/present. Retain compatible HDR and audio;
compare resolutions within the same codec/range route before considering a
color-grade fallback. Prefer the smallest standard candidate that covers the
fitted picture within the proposed tolerance in §3.2 as the initial policy
hypothesis. Higher resolutions remain selectable manually; Auto does not spend extra encoding work solely to exceed the fitted
picture. Treat this as a useful-resolution policy, not a claim that all
higher-resolution encodes are perceptually identical. The rendered-picture
comparison in §8 is a decision point: if a larger sustainable encode delivers
a material visible improvement on the target display, revise the preference
before qualification rather than defending the smaller rung by definition.
Keep that outcome explicit in the policy fixture and bitrate decision record;
The Opus review has challenged this trade-off; §6.1 records its disposition.

If resource or network constraints exclude that candidate, choose the highest
sustainable candidate below it. Unknown display dimensions retain existing
source/route behavior. Unknown network capacity is not evidence that original
playback must be downgraded. A known failing decoder/route remains excluded
until the scoped retest condition is met.

```text
Original compatible and sustainable? ── yes ──> direct/video-copy
                 │ no
                 v
Resolve encoder + codec/range + subtitle graph + output decoder limits
                 │
                 v
Build supported candidates, with exact geometry and wire cost
                 │
                 v
Choose display-covering candidate, bounded by measured supply/decode limits
                 │
                 v
One playback owner observes, prepares, commits, or retains the incumbent
```

### 3.2 Fit the picture, not the panel's short edge

Use the active video container in physical/backing pixels. For display aspect
`a` after rotation and sample-aspect correction, container `W×H` gives:

```text
fit_width  = min(W, H × a)
fit_height = min(H, W / a)
```

Evaluate candidate output width **and** height after the production scaler's
rounding and aspect treatment. Select the first supported candidate within the coverage tolerance on both
fitted dimensions; never encode an upscale of source content. Proposed
`max_display_upscale = 1.10` means `fit_width/output_width <= 1.10` and
`fit_height/output_height <= 1.10`, after rounding. This allows at most 10%
local enlargement, not a 10% shortfall (which would allow 11.1% enlargement).
It is a shared fixture constant and a product choice in §6.1, not a decoder
limit or permission to hide manual rungs. Keep source-coded
geometry separate from display geometry. Baked-in black bars remain part of
the coded frame; detecting and cropping them is out of scope.

| Source / presentation | Expected Auto transcode preference, before pressure |
|---|---|
| 3840×2160, 2400×1600 container | 2560×1440; fitted picture 2400×1350 |
| 3840×1608 cropped scope source, same container | Fit about 2400×1005; choose a candidate using its actual output width, not a 2160p badge |
| 3840×2160, 2000×1200 container | 1920×1080 under the proposed 1.10 tolerance; fitted picture 2000×1125, about 4.2% enlargement |
| 3840×2160, portrait 1080×2340 backing container | Fitted picture 1080×607.5; eligible 720 candidate, not a 2160 encode justified by container height |
| 1920×1080 source, same container | At most source dimensions; no synthetic 1440p upscale |
| 3840×2160, 1920×1080 container | 1920×1080 encode; compatible original still allowed |
| 3840×2160, genuine 3840×2160 output | 2160 candidate if route and sustained supply permit |
| Unknown, zero or hidden presentation rectangle | No display-derived cap; retain established choice, do not infer a tiny screen |
| Portrait, PiP, split screen or external display | Fit the active surface, preserve orientation; debounce changes before voluntary switching |

For scope movies, an initial implementation may select the standard 1080
height candidate whose aspect-preserving width covers the fitted picture.
Do not relabel its actual geometry as 1920×1080. Per-width or 16:9-equivalent
rung redesign is outside this effort; tests must make this distinction clear.

### 3.3 One candidate builder, separate capability and policy views

Do not append 1440 blindly to `LADDER_HEIGHTS`: that array also serves offline
and pre-route APIs. Introduce a route-aware candidate builder used by session
planning, live advertised choices, Auto starts, prior evaluation, recovery and
upgrade selection. Keep the offline contract stable. Pre-route decisions may
advertise only facts they can prove; the player refreshes its menu from the
resolved session rather than remaining stuck on a conservative decision list.

The capability list answers **what this source/worker/client can serve**.
Presentation size and temporary network pressure choose within that list;
they must not erase valid manual choices. Filter HDR and decoder ceilings per
candidate, not via a single ceiling that implies every lower height works.

Retain existing wire fields `height`, `total_kbps`, `peak_kbps`. Workstream A adds
only response metadata proven compatible by its consumer census, such as exact
width and codec/range; a conservative old-client projection still carries the
existing rung shape. Workstream B adds the negotiated candidate identity (§4.2).
Fields describe actual output and **resolved audio plus video**, not source
bitrate. No early candidate ID in a strict retained response before B-R1.

Rate selection uses the resolved candidate's width, height, rational fps,
codec/range and rate-policy version. Start with an explicit new 2560×1440
H.264 SDR profile at **12,000 kb/s for 24/30 fps**, then measure at 10/12/16
Mb/s before selecting the final target. Qualify a separate 60 fps profile;
its rate must be measured and stored separately, not silently borrowed from
24 fps. This is a profile table, not a universal linear pixel-rate formula.
Pin all legacy rates outside the new profiles initially. In particular,
`bitrate_for_height`'s existing 8,000 kb/s rate for 1440–2159 source-height
recipes remains unchanged until an explicitly versioned, measured correction
is approved; do not change all scope-film rates by moving one match boundary.
Include 3840×1600 source-rung burns in comparisons. This preserves a legacy
limitation visibly, rather than hiding an unrelated rate change inside 1440.

Compare resolutions at equal **total wire cost** as well as final intended
production rates: 1080 versus 1440 at matched rates, then 1440 versus 2160.
Keep codec/grade/audio/frame rate constant in each pair, include grain, motion,
animation and scope, render all to the fitted target, and retain both objective
metrics and blinded visual comparisons. Determine the final profile and any
legacy-rate correction from those results before release. Rate-policy changes
must change full recipe identity so old cached bytes cannot masquerade as new.

Name both branches of `auto_height_from_prior` in this work: the starved-rung
branch and sustained-kbps branch consume the route's candidate list, not
`LADDER_HEIGHTS` or `ladder()`. Include 2160→1440 starvation (if supported) and
peak-fitting sustained-bandwidth fixtures. A 12 Mb/s video target with the
current 1.5× peak plus 160 kb/s audio costs **18.16 Mb/s peak**, so 14 Mb/s
cannot select it under this policy. Use 20 Mb/s→1440 and 14 Mb/s→1080 fixtures
for that profile, with the runtime 0.95 safety factor applied where applicable.
If measurements choose different rates, derive expectations from their actual
peak, not from the nominal video target.

A-05 cause provenance governs new negative network priors: only fresh `link`
evidence may write `worst_rung_height`; encode, decode, hold and authority
cannot poison it. Since old rows/events conflate network and supply, retain
legacy semantics for old clients but mark provenance explicitly for new policy
records and do not import unattributed legacy negative verdicts as new-policy
network proof. Reader compatibility, expiration and coexistence belong in the
storage/telemetry tests. Workstream A needs this narrow attribution boundary before
using a negative prior; it does not need a native runtime controller.

**Existing complete cache entries are candidates.** Inspect compatible complete
pretranscodes by full source revision/recipe, selected audio/subtitle treatment,
codec/grade and actual dimensions. A complete eligible 2160 recipe with equal
or better fidelity, sustainable wire cost and decoder load beats starting a
live 1440 encode: incremental encoding cost is zero. Incomplete, expired,
wrong-grade, wrong-track or inaccessible-worker entries do not count. Network,
decoding and storage delivery still have costs; zero encode cost is not proof
of smooth playback. No pre-encoded SDR outranks a sustainable compatible HDR
original solely because it is cached. Leave speculative pretranscode target
policy (`pretranscode_target_height_for` in
[construct.rs](../../crates/plurxd/src/transcode/manager/construct.rs)) unchanged;
consume what exists without generating every rung in advance.

### 3.4 HDR, frame rate and width are admission constraints

`capability_height_for_encoder` currently caps ordinary hardware HDR sources
at 1080, software at 720, with a special QSV Profile-5 exception. Adding a
1440 rate alone cannot change the tablet's HDR-source result. Workstream A replaces
that blanket ceiling for new policy with per-route candidate admission and
separately qualifies:

| Source → output | Required proof before advertising 1440 |
|---|---|
| SDR → SDR | Production encode at actual fps/dimensions, with and without required burn. |
| HDR10 → tone-mapped SDR | Actual decoder, tone-map filter and encoder on each worker family, with and without PGS burn, under one other admitted session. |
| Supported DV profile → tone-mapped SDR | Separate proof per DV route/profile (including applicable Profile 5/7/8 handling), encoder and burn graph; no inference from the QSV Profile-5 result. |
| HDR10 / supported DV → preserved HDR10 | Correct metadata/sample entry and realtime proof for 1440; no inference from the existing 1080/2160 points. |

M0-A records the TCL model, OS/build, panel HDR support, codec profiles/levels,
frame-rate limits and dual-decoder capability. Treat its common HDR-source
paths as named acceptance rows; an SDR-source test cannot stand in for them.
Measure the relevant tone-map graphs before client M3-A work. Admit 1440 only
on workers whose exact route passes (heterogeneous capability is the default
technical choice); others retain a proved lower fallback with
`capability_limit`. Revalidate on worker reassignment; a capable ingress is
not a capable encoder. Report failure to sustain the tablet's HDR→SDR 1440
route explicitly before proceeding with the client milestone; never label an
SDR-source-only success as solving that limitation. Audio incompatibility
alone, including TrueHD, is not a reason to transcode compatible video.

Qualify intermediate HDR on each supported production route before advertising
it: ffmpeg arguments, bit depth, transfer/primaries/matrix, HDR metadata,
container/sample entry, truthful codec level, and sustained realtime speed.
A boot graph proof establishes support, not spare capacity under concurrency.
Run measurements at 24/30/60 fps and the supported subtitle-burn paths.

If 1440 HDR is unavailable but 2160 HDR is sustainable, prefer that over an
unnecessary SDR conversion. If only 1080 HDR is sustainable, choose it before
a speculative 1440 HDR request. SDR-only displays still require the existing
correct conversion. Explicit dynamic-range choices retain existing refusal
semantics; Auto fallback must be reported rather than mislabeled HDR.

Current Android probing at 30 fps cannot justify 60 fps. Extend output-candidate
checks to actual width, height, profile and frame rate using available decoder
capabilities, then validate on hardware. For clients without reliable rate
claims, retain existing conservative behavior and measured decoder limits;
never turn absent information into a fabricated 60 fps proof.

## 4. Protocol, state and ownership

### 4.1 Presentation facts are optional and do not grant codec capability

Proposed `presentation_target` object:

```json
{
  "width_px": 2400,
  "height_px": 1600,
  "revision": 3
}
```

These dimensions describe the active render container, before letterboxing,
already converted into backing pixels. Do not send CSS dp/points or multiply
density twice. Accept positive integer axes up to 16,384; absent, zero or
out-of-range rectangles yield unknown presentation, not a playback refusal.
An unknown rectangle must never enable an otherwise unsupported route. Do not
persist these measurements as a device fingerprint or network-prior key.

There are two compatibility surfaces. Create-time `DisplayCaps` in
[caps.rs](../../crates/plurx-core/src/playback/caps.rs) already tolerates unknown
fields. Workstream A can send the optional target there without adding fields to
strict control or remote-start envelopes. The selected concrete legacy recipe
crosses a remote boundary; an old worker must never receive new fields or be
assumed to understand new geometry/grade semantics. Reuse its proved legacy
arguments or exclude it for that candidate. Target omission and ignore-by-old
server both preserve the old behavior. Response/menu additions need their own
consumer census; permissive caps parsing is not a blanket wire guarantee.

Runtime extensions are Workstream B and require **three separately deployable
releases**, following the withdrawn `accepted_acknowledgements` lesson in
[streaming reliability §4](STREAMING-RELIABILITY-HANDOFF.md):

1. **B-R1: parser compatibility only.** Tolerate the planned optional additions
   on every relevant nested input, relay, response and replay type; send and
   advertise none. Include `ControlRequestV1`, `ControlRelayRequest`,
   `QualitySelection`, `DesiredQuality`, `MediaIntentEnvelope`,
   `RemoteStartRequest`, `ControlResponseV1`, the relay response validator, and
   `RetainedTerminalResponse`/stored response JSON. Limit tolerance to the
   planned compatibility contract; do not weaken authorization, enum validation
   or required-field semantics. Deploy this release fleet-wide and record the
   versions of every ingress, owner and eligible worker.
2. **B-R2: semantics and advertisement.** Implement route requests and retained
   target handling. Advertise the version in the owner-minted start response,
   not a newly extended control reply parsed by old relays. Advertisement is
   permitted only after a fleet-wide B-R1 receipt for the current membership;
   the owner's own binary version is insufficient. Distinguish parser support
   from ability to honor a candidate. Require semantic support on its actual
   owner/worker before dispatch; mixed B-R1/B-R2 fleets fall back to legacy
   eligible routes or decline the new candidate without touching the incumbent.
3. **B-R3: clients send the extensions.** Send them only for the negotiated
   session and compatible route. Membership/owner changes revalidate support.
   Never silently strip a selected candidate and reinterpret it as Original
   after a newer ingress routes to an older owner. Use an explicit bounded
   fallback/re-plan through the existing owner, retaining user intent.

B-R1 is the ordinary behavior-preserving infrastructure prerequisite described
in §2.1. B-R2 server and B-R3 client artifacts come from the fully qualified
combined feature effort, deployed in that order. Prove the full feature on lab
builds before promotion, then stage production binaries without shipping an
unqualified partial feature. R3 activation waits for R2 route semantics and
compatibility evidence. Before rolling back,
stop new extension use, invalidate support on affected sessions and retain the
B-R1 parser floor wherever new requests or terminal JSON may be replayed.
A rollback below that floor requires draining or a tested conversion of the
new retained state and outstanding requests; merely toggling support off does
not make old strict parsers compatible. Test both the safe rollback and refusal
to proceed below its floor. No client retry loop on `503 control_unavailable`.

The B-R1/B-R2 design census lists every serialization/retained-context path and
its versioned fixtures. Required mixed-fleet rows: old ingress→new owner,
new ingress→old owner, new→old remote start, and new terminal response replay
after rollback. Here “old” must identify pre-B-R1 versus tolerant B-R1: tests
prove pre-floor peers receive no new wire shape, not that they magically parse
it. Capability receipts are operational compatibility evidence, not Developer
readiness gates; they never reject saving the user's setting.

Carry one normalized presentation target through initial planning and retained
preparation context. Runtime target revisions increase within a client-instance
and playback generation, and stale revisions cannot undo newer targets. A
rectangle change is policy input, never a manual quality selection. Workstream A
recaptures at a new create; Workstream B adds live revision-driven adaptation.

### 4.2 Preserve the playback-control owner

Initial selection is server-owned. Runtime policy is evaluated on each client,
where throughput, rendered frames and visible surface are observed, using a
shared set of language-neutral conformance fixtures. The server validates every
candidate and remains the owner of the prepared successor and admission.
Settle the existing fixture disagreements first (§2.1), then extend web policy
and the A-05 pure reducers on Android and Apple. Do not add an independent server ABR timer.

`Auto { height: Some(...) }` alone cannot command a delivery-route change:
`plan_preparation_candidate` in
[preparation.rs](../../crates/plurxd/src/http/hls/preparation.rs) resolves copy
compatibility separately, and the legacy candidate path preserves the method.
A compatible 4K original can otherwise ignore an automatic 1440 request.

In B-R2/B-R3, add a negotiated optional `candidate_id` to the Auto request
vocabulary in both daemon `QualitySelection` and core `DesiredQuality`, with
an equivalent initial-create representation when required by a runtime re-plan.
Use a `CandidateId([u8; 16])` value type, encoded as exactly 32 lowercase hex
characters on the wire; validate malformed IDs. The value and its optional
wrapper remain `Copy`, preserving existing by-value enum matches. M0-B freezes
the canonical bytes/digest domain and defines collision/lookup handling; the
ID is not a truncated hash accepted without full recipe validation. Workstream A
requires no new Auto candidate request. The server-issued identity binds the source
revision, delivery method (copy/encode), output geometry, codec/grade and
encoding recipe version. It is a requested route, never an authorization token;
resolve it server-side against the current authenticated playback, selected
tracks, worker and capabilities. Invalid or obsolete identities are explicitly
refused/replanned under the same owner, never silently interpreted as original.
Missing identity preserves legacy height-only behavior. New clients only send
it after negotiated support; include it in desired canonicalization/digest
when present while preserving old digests byte-for-byte when absent.

A copy candidate carries the source route, not a synthetic encode height.
An encode candidate must actually force that validated route even if the
source is otherwise decodable. This supports copy→1440 encode→copy without
pretending any step is Manual. Fixture coverage must exercise both ordinary
and retained/legacy preparation paths, supersession, retry, old-server
fallback and worker revalidation. On an old server retain existing recovery;
do not claim seamless cross-route Auto support.

Automatic requests remain Auto, now with an optional validated candidate;
user actions remain Manual or Original. Feed the resulting desired change through the existing
sequence/generation fences, prepare, commit and cancellation flow. Rectangle
updates alone do not mutate the user selection digest. Once policy chooses a
new Auto candidate, its height and route identity identify that specific ask.
A rectangle revision invalidates an uncommitted obsolete automatic proposal;
manual supersession always wins. Replay must reuse the same decision and
request identity rather than recomputing another step down.

All paths that can replace playback share one arbitration point: quality,
decode rescue, supply recovery, track/subtitle changes and user actions.
User actions supersede automatic work; emergencies cancel an obsolete upgrade.
A held or failed preparation must release its claim. Keep the incumbent until
the successor has proved overlapping media position, buffered runway and
presentation. A dual-player declaration is necessary but does not prove the
device can decode two 4K streams at once; measure the handoff workload.

A failed voluntary upgrade keeps the incumbent. A stalled incumbent can use
one bounded reopen through existing recovery budgets. At the ladder floor,
retain the established terminal behavior rather than generating endless
successors. No new parallel playback store or unconditional second encoder.
Temporary successor encoding must use existing resource admission.
Voluntary candidates use `Encoding.speculative` / `Priority::Speculative` in
[vodencode.rs](../../crates/plurxd/src/vodencode.rs) until commit, with an
explicit voluntary reason preventing foreground/Live TV preemption. Speculative
priority alone is insufficient: today's handoff path may ask the predecessor
to yield a permit. A voluntary Auto preparation must never use that path,
register a foreground waiter, release the incumbent's permit, or fall back to
an interrupting reopen because all slots are occupied. Cancel and keep playing.
Emergency downshifts keep the existing recovery priority/budget. Pin a fixture
with Live TV plus an incumbent occupying all slots: the voluntary successor is
refused, neither foreground session yields, and the incumbent stays unchanged.
Do not alter global admission policy or pretend slot counts measure pixel cost.

### 4.3 Runtime evidence and proposed thresholds

Reuse the web controller's existing constants unless measurements justify a
change; port behavior rather than merely copying their numeric values.

| Signal / action | Contract |
|---|---|
| Sampling | Client decisions at most once per second. Web policy status reads remain at most once per 5 seconds. Preserve Apple's existing 2-second recovery/preparation polling and other owner cadences; reuse their observations rather than adding polls or slowing them. Cause and transfer evidence expire after 15 seconds. |
| Severe supply pressure | Fresh supply evidence and draining buffer trigger a direct move to a candidate whose peak fits 0.95× the conservative fresh estimate; skip intermediate rungs when necessary. |
| Mild pressure | Two consecutive low-headroom samples can step down once; preserve the effective `max(cooldownMs, dwellMs)` = 60-second voluntary gap and restart-cost test. The 20-second constant alone is not the live gap. |
| Upgrade | At least 45 seconds of sustained headroom, no stall in 60 seconds, at least 10 seconds runway; at most one upgrade per 60 seconds and no upgrade within 90 seconds of a bandwidth cliff. |
| Upgrade safety | Use measured route production margin (initial floor 1.15× realtime), actual candidate cost, recent transfer evidence and decoder evidence. Height-squared cost prediction is only a same-route heuristic, never proof across codec/range/burn changes. |
| Decode pressure | Confirm rendered-frame loss with elapsed presentation time and adequate supply; a waiting callback alone is not a decode failure. Scope failures to codec/profile, dimensions, frame rate, bitrate band and route. |
| Missing telemetry | Unknown is neither healthy nor failed. Preserve a healthy incumbent; do not fabricate bandwidth or decode headroom for an upgrade. This deliberately tightens the current web `predictedSpeed(null)` allowance, so it needs a fixture change and an evidence/retest path, not a mechanical port. |
| Pause/seek/hidden/scrub | Freeze evidence windows and discard contaminated samples. Resume cannot inherit a fictitious 45 seconds of healthy playback. |
| Surface resize | Require a stable target for 5 seconds; normal voluntary cooldowns still apply. A resize never interrupts healthy original playback by itself. |

Separate network, producer and decoder explanations. JIT fragment throughput
is constrained by both encoding and transfer; a fast current encode does not
prove the next encode sustainable. Network starvation and decoder failures
must not poison each other's learned limits. Existing coarse node-local priors
remain hints; no new cross-device quality ceiling is persisted under their key.

**Producer headroom excludes deliberate pacing.** A reader-paced VOD producer
can report 1.0× average delivery while having substantial spare capacity; that
wall-clock rate must not block every upgrade. Use a fresh active-production
measurement: media seconds encoded divided by active encode wall seconds,
excluding explicitly measured pacing intervals, startup, seek and queue waits.
Require at least two media segments and 2 seconds of active work within the
last 15 seconds before treating it as known. Record source route/fps/graph and
worker with the sample. Use fresh unpaced catch-up intervals from the existing
producer instrumentation; M0-B proves that those intervals exclude pacing.
If timing is unavailable, headroom is unknown; do not manufacture 1.15× from
idle time alone. Pin cases for a paced producer delivering 1.0× overall but
2.0× while active, an actually saturated producer, and missing pacing timing.
Copy/cached candidates require delivery and decode evidence, not encoder
headroom. This eliminates an inappropriate encode-speed test for original.

**Deterministic recovery at natural playback boundaries.** The recommended
Workstream B baseline re-runs original-first cold-start selection on a completed
viewer seek, resume after at least **60 seconds** of explicit pause, and next
episode. Manual/Original retain their intent; automatic repair/retry, seeks
used internally for synchronization, scrubbing previews and backgrounding alone
are not user discontinuities. Coalesce scrub events to the final committed
seek. Cancel obsolete automatic work and resolve within the existing owner
transaction at the requested position; at most one attachment/change occurs.
A same-session seek or resume is not already a reopen, so measure any added
startup cost rather than claiming the route change is free. If the alternate
route cannot be ready under the existing transaction deadline, fulfill the
user action with the current healthy route; never add a second fallback switch.

Fresh transfer evidence (at most 15 seconds old) may veto an unsustainable
original at this boundary. Old cliff verdicts and expired transfer samples
must not pin the reduced route; decoder/capability exclusions and explicit
user/network limits still apply. A fresh proven link limit is not erased by a
seek. Do not globally clear shared network priors: recompute this Auto start
using age and provenance. Unknown network capacity again means no evidence
against compatible original, as at a real cold start. Test a transient cliff
followed by a seek after its evidence expires→copy, a seek while fresh bad
network evidence remains→reduced encode, and repeated user seeks without
multiple competing successors. This does not guarantee a second network
failure cannot occur; it provides a concrete opportunity to recover quality.

**Safely proved mid-play return is part of Paul's chosen scope.** Natural
boundaries are a deterministic fallback, not the only recovery opportunity.
After the normal 45-second healthy window, 60-second stall-free interval,
90-second post-cliff hold and 10-second incumbent runway, re-evaluate at most
once per 60 seconds. A timer permits evaluation, never a blind route switch.

Compare source/cached candidate peak wire cost and actual codec/profile/fps
against fresh usable delivery evidence. Copy/cached routes have no encoder
margin requirement. If compatible original has qualified supply/decode evidence,
prepare it through the existing owner. If that evidence is missing, allow one
bounded **speculative candidate preparation** to measure its actual delivery
and presentation while the incumbent remains authoritative. This must use the
existing authenticated source range/segment path, not a full duplicate movie
download, and hold at most one successor within existing byte/runway/deadline
bounds. The implementation must expose *actual transfer active time* and known
bytes, not wall time dominated by producer pacing, so its bandwidth sample is
meaningful. Original source peak/segment costs must come from probed or measured
media, never the low-rate encode's metadata.

The candidate must meet existing continuity commit conditions, sustain fresh
peak headroom, and present/decode correctly while the incumbent keeps adequate
runway. Abort on incumbent runway falling below 10 seconds, active pressure,
insufficient dual-decoder capacity, expired source-cost evidence, or admission
refusal. Do not release the incumbent's permit, preempt Live TV, or turn a
failed voluntary preparation into an interrupting reopen. After failure, wait
at least five minutes and require fresh evidence before the next trial; after
success the normal dwell/cooldown applies. On a backend unable to probe original
under those bounded conditions, retain the encode and log a specific missing
proof instead of claiming automatic restoration has succeeded.

M0-B must demonstrate that this bounded path can acquire the needed evidence
on the paced encoded-VOD and original delivery paths, including a candidate
whose cost is far above the incumbent's. This is a **blocking build prerequisite
for mid-play return**, not an optional future experiment. B3/B5 require a
long-title trace: one transient cliff, no user seek/pause, link restored, safely
proved return to original with no foreground interruption. Natural-boundary
recovery remains available if transient admission/measurement conditions delay
a trial, but cannot substitute for this acceptance row. An inability to prove
the path leaves the combined objective incomplete and must be reported to Paul;
it does not authorize silently reverting to boundary-only recovery.

### 4.4 Source facts and per-platform telemetry have owners

The current `MediaFile` geometry and `output_size` are insufficient to promise
rotation/SAR/frame-rate correctness, and `VideoCaps` cannot express all output
constraints. Source fps already exists in `DecodeFacts::frame_rate` in
[decode.rs](../../crates/plurx-core/src/transcode/decode.rs); reuse that reader
rather than inventing a second fps probe or automatically adding catalog
columns. M1-A owns a normalized source-facts adapter at that seam, supplying
coded geometry, rational SAR, rotation, display geometry and rational fps to
initial planning, resolved candidates, retained preparation and remote-worker
requests. Cache facts by source revision with the existing probe/cache
lifecycle. M0-A chooses any additional storage only if those facts cannot survive
existing transport; a new durable schema then requires migration, old-row and
replicated-store regression coverage.

For these new candidates, normalize rotated/anamorphic inputs to upright
square-pixel output in the production filter graph. Derive planned geometry
from that same transform; explicitly account for ffmpeg autorotation so the
picture rotates once, not twice. M2-A must emit matching SAR/rotation/container
metadata and manifest dimensions on both rolling and immutable VOD paths.
A planner-only calculation with an unchanged coded-aspect encoder is invalid.
Unknown rotation/SAR/fps retain the established route, suppress unsupported
geometry/rate-derived claims, and are reported as unknown rather than assumed
square/30 fps; reacquire facts under the existing bounded probe deadline.

M1-A also defines nullable decoder width/rate/profile constraints in permissive
create capabilities, with compatible remote-worker validation. Runtime updates
belong to B-R2. M3-A/M4-A report only limits actually probed by each client. A standard
height probe is not permission for an arbitrarily wide scope frame. Known
constraint violations must refuse that candidate; missing new constraints on
old clients retain their legacy behavior without being recorded as a new proof.

Android uses a per-playback bandwidth-meter handle, not a process singleton
contaminated by downloads or Live TV. Fresh transfer samples, smoothed link
estimates, status age and rendered-frame counters are distinct inputs.

Apple currently lacks a proved fresh per-transfer sample (A-04 §8.4.1).
Access-log `observedBitrate` is not a substitute for fresh cliff evidence, and
`numberOfStalls` is not a decoder-failure counter. M0-B must measure whether
access-log deltas or an existing transport measurement can supply the needed
sample. Without it, Workstream A can deliver display-aware initial selection, but
the combined cross-platform cliff/recovery objective is **incomplete**; do not
mark its §8 rows passed with N/A. Resolve a real adapter or obtain a separately
documented scope decision before completing the combined effort. Paul chose to
retain this dependency rather than ship A independently.
A-05's cause-class and owner rules govern actual decoder failures.

## 5. User experience and rollout

Auto remains the normal viewing choice. Settings describes the saved policy;
the in-player menu shows current session-supported choices and the effective
Auto result, for example `Auto · 1440p`. 1440 must be selectable in the player
when supported, even when the screen is smaller or the network is temporarily
slow. Keep existing saved choices intact; unavailable preferences are shown
honestly with the resolved outcome, not silently changed in storage.

Playback info distinguishes source, delivered dimensions, rendered range,
selected policy and reason. Proposed stable reason codes include
`original_compatible`, `display_fit`, `network_pressure`, `encoder_pressure`,
`decoder_pressure`, `capability_limit`, and `recovery_upgrade`. Record candidate
cost, target dimensions, evidence age, previous/new route and switch outcome.
Counters without covered time are not evidence: show missing measurements as
unknown. Do not expose session bearer tokens in exported evidence.

Workstream A gets its own Settings → Developer card, **Fit Auto to the display**,
with proposed saved key `playback.display_aware_auto` (default off while
incomplete). It controls only new Auto starting-selection policy, including
re-plans performed under that policy; manual 1440 and truthful capability
metadata are available independently. Older clients lacking a target keep
existing behavior. Readiness rows name tablet SDR-source, HDR→SDR routes,
actual geometry, cached-recipe reuse, manual continuity and client compatibility.
They are advisory: never disable the switch, reject Save or override a saved
choice. Expose supported status and effective policy in diagnostics.

A's card graduates only when M5-A's matrix and the combined final qualification
pass on the exact promotion candidate. Move
the useful permanent control into Playback, preserving its saved value; remove
it only under an explicit migration decision that handles prior opt-outs.
The existing `playback.auto_abr` card and A-05 ownership remain unchanged for
runtime policy. Workstream B uses that switch, preserving its saved state and
existing per-platform evidence requirements. No readiness-based runtime gate.

Rollback uses both controls as applicable: disabling
`playback.display_aware_auto` stops new display-aware initial/re-plan selection;
disabling `playback.auto_abr` stops runtime automatic adaptation. Turning off
the former alone does not stop the latter. To stop all newly introduced Auto
behavior, disable both while preserving the viewer's current healthy route;
retain manual 1440, compatibility and truthful manifests. A server
rollback must follow §4.1's parser floor, state-drain and support invalidation
contract, not merely ask clients to omit fields after strict peers fail. Any
persistent byte cache is keyed by the full encoding recipe (including changed
bitrate/grade), never just a height; a new rate must not reuse old bytes under
the same identity.

## 6. Decisions and alternatives

1. **Original first:** preserves source quality and avoids unnecessary server
   load. Trade-off: a cold network can still require later rescue.
2. **Standard 1440 before custom 1350:** useful coverage with bounded profiles,
   cache identities and qualification work. Accept a small amount of local
   downscaling on the 2400×1600 tablet.
3. **Route-aware candidates:** a global height list cannot express a missing
   HDR rung or different codec limits. Accept a larger planning change to
   avoid advertising streams that will fail.
4. **Client policy, server admission, existing owner:** preserves low-latency
   local observations and established cancellation. Accept three reducer
   implementations, constrained by shared fixtures and integration tests.
5. **Conservative upgrades:** smoothness outranks chasing a higher label.
   Lower quality is temporary when sufficient evidence becomes available;
   unknown evidence does not authorize repeated exploratory interruptions.
6. **Measured bitrate and HDR qualification:** nominal 1440 support is not
   enough. Accept a qualification milestone before treating it as finished.

Rejected alternatives: force 1440 globally (damages good originals); append
1440 to every shared array (changes offline behavior and unproved routes);
exact screen-size encodes everywhere (unbounded variants); start every title
at 1080 and climb (unnecessary quality/startup churn); server-only adaptation
(loses presentation evidence); simultaneous full ladders (multiplies GPU load).

### 6.1 Product choices recorded for this revision

Paul answered these product questions on 2026-09-30; these are decisions, not
pending recommendations:

| Choice | Paul's decision | Consequence |
|---|---|---|
| Delivery split | **Keep one combined effort** | A-05/native and Apple evidence block the full promotion; no independent release of initial-selection improvements. Compatibility-only prerequisite releases remain technically necessary. |
| Coverage tolerance | **Allow up to 10% enlargement**, encoded as `max_display_upscale = 1.10` | 2000×1200 can choose 1080; 2400×1600 still chooses 1440 for required 16:9 encode. |
| Return to original | **Also during playback when safely proved** | Both natural-boundary re-plan and bounded mid-play restoration are required. Failed voluntary trials preserve the incumbent; lack of an attainable proof is a blocker, not a reason to drop this scope. |

A technical decision does not require a fourth product question: use proved
per-worker tone-map candidates rather than unnecessarily capping the whole
fleet to the weakest worker. A failed proof still means that worker cannot
serve the rung. M0-A must report the actual tablet/fleet result plainly.

## 7. Milestones, ownership and integration

### 7.1 One integration branch; workstream A supplies starts and menus

After the parser-only prerequisite is settled, use
`effort/display-aware-auto-quality`, based on freshly fetched authoritative main
recorded at M0. Each task branch starts from current effort and targets it.
Files overlap: the disjoint-file exception does not apply. M5-A is a workstream
acceptance checkpoint, not permission to promote. Freeze, integrate main and
qualify the **whole** effort only after A and B pass. Native-controller and
Apple measurement dependencies are explicitly on the release critical path.
M0 may run the evidence experiments before opening the long-lived branch to
avoid carrying implementation while a measurement prerequisite is unresolved.

| ID | Work / primary files | Depends on | Acceptance |
|---|---|---|---|
| M0-A | Current-main SHA/census, tablet model/panel HDR and decoder profiles/levels, route/burn inventory, cache/manifest/manual continuity dependencies; fix the Workstream A protocol boundary | None | Record actual facts and settings; select representative SDR/HDR10/DV corpus; no new strict control fields; identify qualifying workers and affected clients. |
| M1-A | Optional create target and decoder constraints in core `playback/caps.rs`; DecodeFacts adapter and geometry normalization; existing request/remote-worker compatibility; Developer card/key | M0-A | 2400×1350 fitted picture; tolerance and portrait fixtures; old/new creates and mixed-worker fallback; unknown facts preserved; saved switch obeyed with no readiness gate. |
| M2-A | Candidate builder replacing new-policy `capability_height_for_encoder` ceiling; both `auto_height_from_prior` branches; link-provenance boundary; explicit rates; complete-cache reuse; manager plan/describe/create/construct and recipe/manifests | M1-A | SDR-source and HDR→SDR 1440 proofs distinct; per-profile DV/burn qualification, exact peak metadata, 20 vs 14 Mb/s fixtures, encoder-stall prior isolation, legacy scope rate pinned, cache identity/reuse tests. Report actual HDR limits before M3-A. |
| M3-A | Android create target, correct decoder/rate claims, live-menu refresh and initial policy: `Caps.kt`, models, `Controller.kt`, `PlaybackPolicy.kt`, `PlayerScreen.kt`, Settings | M2-A | Actual tablet keeps healthy copy, selects eligible 1440 on each qualified source-grade route, exposes manual 1440, preserves audio/subtitles. Unsupported tone-map result is a named fallback, not an SDR-only pass. |
| M4-A | Web and Apple create-time target plus menu refresh; replace web height-only initial fit input with two-axis fit; reuse any current web runtime reducer without adding candidate-route protocol or native ticks | M2-A | All initial geometry/compatibility fixtures pass, including portrait web; existing web Auto never invalidates the initial choice by treating container height as picture height. Any affected existing web behavior passes its current D3 regression oracles. No new native controller. |
| M5-A | Source-grade/device measurements, matched-rate comparisons, rate choice, manual-switch continuity, cache/thermal/concurrency, docs and Developer graduation | M3-A, M4-A | All A rows in §8 pass on the exact candidate, actual fallback limitations recorded, affected builds pass. Hold feature promotion for B5 and the combined gate/receipt; no independent A release or claimed runtime parity. |

If normalized geometry cannot be conveyed to an older worker through proved
legacy semantics, exclude that candidate there; do not widen strict remote
messages inside A to save a fallback. Additional protocol evolution belongs to
B-R1. A may use the server's existing resolved recipe path for ordinary 16:9
1440 while retaining current behavior for unknown/unproved special geometry.

### 7.2 Workstream B — A-05 inside the same effort; staged deployment

Claim/update the **A-05** board row before runtime edits. Amend its milestone
ledger to map the following slices to the same combined effort. B-R1 alone is
an ordinary compatibility-only main-bound prerequisite, with its own review,
focused regression and affected-surface gate, deployed before activation.
All behavioral tasks B-R2 onward target the combined effort. R2/R3 are staged
server/client **deployment releases from the same fully qualified feature**,
not permission to merge unfinished behavior. Include mixed-version and staged
activation rehearsals in lab qualification before promoting the effort.

| ID | Work / primary files | Depends on | Acceptance |
|---|---|---|---|
| M0-B | A-05 fixture disagreements, current-main census, exact candidate/target schema, strict relay/replay inventory, active-production telemetry and Apple fresh-sample experiments | A's schema/candidate contract; A-04/A-05 ownership | Fixed-width candidate ID/digest contract; settled shared fixtures; actual platform evidence paths recorded before adapter work. Apple uncertainty blocks its adapter, not B-R1 compatibility work. |
| B-R1 | Tolerance-only server release on all nested control/intent/remote/replay types | M0-B wire census | Mixed-version fixtures; no fields emitted or advertised; ordinary behavior unchanged; deployed fleet receipt including every ingress/owner/worker. |
| B-R2 | Route semantics, owner start advertisement, server admission/replay and capability invalidation | B-R1 fleet receipt | Only semantically capable routes advertised/dispatched; safe rollback-floor and retained replay tests; no strict relay response advertisement. |
| B-R3 / B3 | Negotiated client requests and Android runtime reducer/reporting/owner integration; attributed recovery, original re-plan at natural boundaries and proved mid-play restoration, speculative upgrades | B-R2 eligible route support; A-05 settled fixtures | Copy→encode→copy stays Auto; stale IDs/manual supersession; paced-headroom fixtures; seek-after-cliff and no-seek mid-play original recovery; fresh-limit veto; full-slots refusal preserves incumbent; Android shaped trace before controller PR merge. |
| B4 | Web runtime extension and Apple adapter, shared fixture runners, target revisions and protocol fallback | B3; Apple fresh-transfer solution before Apple tick | Existing D3 oracles unchanged, old-server/mixed-fleet behavior passes, no fabricated Apple emergency evidence; each platform trace precedes its controller merge. |
| B5 | Full runtime qualification, recovery/intent/admission evidence, rollout/rollback and A-05 Developer lifecycle | B3, B4 | A and B §8 matrices pass on the exact combined candidate, including no-seek return to original; promote with complete gate/receipt, then deploy R2 server before R3 clients. No partial platform parity claims. |


Before Rust edits, establish the pinned Rust 1.97.1 loop from
[agent compile loop](../ci/AGENT-COMPILE-LOOP.md), verify `rustc --version`, and
archive committed source without `.git` or credentials if using the cloud.
Run check, Clippy with denied warnings, formatting and focused regressions
before pushing; CI is not the compiler. Requalify after rebasing or integrating
a new base. Use `make unit-core` or `--features hiqlite-store` for replicated
storage tests. Commit with the tracked hook intact.

Observable behavior changes use `fix(` or `perf(` subjects. Each task PR names
actual existing regression functions as `Regression-Test: path::test_name`;
copy those lines into landing commits, including `MergeMessageField` for an
API merge. Do not invent a test name to satisfy the field. Obtain the blocking
effort gate, freeze task merges, integrate current main and qualify the exact
promotion tree under the repository's current pipeline and AGENTS rules.
On 2026-09-30 Paul authorized a GPT-6.1 Sol agent to build this reconciled
plan. Follow the compiler loop, task-branch, regression, review and promotion
requirements above; that authorization does not waive evidence, parser-rollout
compatibility or production release policy.

## 8. Verification and completion evidence

### 8.1 Deterministic regressions

Extend existing suites; proposed cases below are obligations, not claims that
new test functions already exist.

| Area | Required cases |
|---|---|
| Geometry | 2400×1600→1440, 2000×1200→1080 under 1.10 tolerance, portrait 1080×2340→at most 720; scope; 4:3; rotated portrait; anamorphic SAR; 1080 source; odd/even rounding; unknown dimensions; hidden/zero container; very large/bad values. |
| Candidate admission | SDR-source vs HDR10/DV tone-map routes separate with/without PGS; per-worker proof/failover; complete cached 2160 reuse and invalid-cache exclusion; per-codec and per-profile decoder bounds; 60 fps rejected when only 30 is proved; 1440 SDR; absent 1440 HDR; worker failover; software-only route; burn graph; live-menu refresh; offline unaffected. |
| Protocol | Old client/new server; new client/old server; omitted extensions; support lost on rollback; stale target revision; repeated request; capability change while preparation is in flight; pre-B-R1 vs tolerant peers at every ingress/owner/remote-start hop; retained terminal replay after rollback; fleet membership invalidation; no silent candidate stripping. |
| Intent/ownership | Auto remains Auto; Manual does not adapt; Original does not silently encode; user seek/track change cancels pending upgrade; one successor; failed admission preserves incumbent; floor budget unchanged. |
| Adaptation | 2160→1440 on mild pressure where valid; cliff skips 1440; no upgrade on stale/JIT-only evidence; held/hidden timer reset; cooldown; failed successor rollback; natural-boundary original recovery after expiry vs fresh-link veto; 60-second pause resume; next episode; no internal-seek trigger; paced 1.0× wall vs 2.0× active work; no encoder-speed condition for copy. |
| Fidelity | Actual encoded dimensions/codec/grade match manifests and playback info; audio channels unchanged when compatible; subtitles remain selected/aligned; seek and A/V sync survive every handoff. |
| Rate/prior/cache/admission | New 1440 24/30 vs qualified 60 fps profiles; legacy 3840×1600 rate unchanged unless explicitly versioned; starvation 2160→1440 when admitted; 20 Mb/s→1440 vs 14 Mb/s→1080 at 18.16 Mb/s peak; encoder stall writes no network negative; full slots refuse voluntary work without incumbent yield. |
| Identity | Fixed-width candidate remains Copy; new bitrate changes recipe/cache identity; target revision alone does not duplicate identical bytes; no poisoned cross-device network/decode prior. |

Useful existing entry points (run affected focused tests first; these commands
are implementation verification, not evidence claimed by the planning pass):

```bash
python3 -m unittest discover -s tests/operations -p test_docs_index.py
node tests/playback/web-policy.test.js
node tests/playback/web-control.test.js
node --test tests/web/settings-sections.test.js
cargo test -p plurxd --lib transcode::tests
make unit-core
make android-test
make apple-test
```

Use the project Android build environment and approved Apple destinations;
record exact commands, filters, source commit, toolchain and test counts.
Focused Android suites include `CapsPolicyTest`, `PlayerPolicyTest`,
`SubtitlePolicyTest`, `PreparedReplacementTest` and reporter tests. Web shell
file additions must update `WEB_ASSETS`, shell tags and the
[layout map](../clients/WEB-SHELL-LAYOUT.md) together.

### 8.2 Physical acceptance matrix

Run the actual TCL tablet with identified model/build and active surface; a
2400×1600 emulator is not equivalent. A's matrix covers initial selection on
the affected Android/Apple/web clients and prepared **manual** switches using
existing machinery. B adds A-04/A-05 runtime traces, including Android TV, iOS
and tvOS. Include actual supported fps, subtitles/burn, scope, cache state,
concurrency and route fallback. An unsupported route is reported explicitly,
not blended into a passing average. Keep workstream evidence distinct but require both in the combined promotion
receipt. No initial-only feature release.

For all affected existing-web and new-runtime cliff/recovery traces, use
**A-04 D3's existing oracle and harness unchanged**, including the 100 ms
maximum presented-frame gap and zero unexpected hitches/stalls/restarts where
that profile requires them. Keep its per-platform baseline, coverage, timing
and unexpected SDR-transition criteria. Do not replace it with a 250 ms or
99.9% aggregate rule. A focused forced-reopen test is reported separately and
cannot excuse a failed prepared/cliff trace. Resolve unavailable platform
instrumentation under A-05; missing data is not a pass.

| Delivery / scenario | Release bar |
|---|---|
| A · Healthy 4K original on tablet, 30-minute run | Original/video copy remains; zero unsolicited quality changes and zero rebuffer events after startup on controlled healthy LAN. |
| A · SDR-source required SDR transcode, 30 minutes | Eligible 1440 measured at 2560×1440 for 16:9; correct aspect/audio/subtitles and zero post-startup rebuffer on qualified route. |
| A · HDR10-source → SDR, with/without PGS burn | On actual tablet SDR output and each claimed worker family, 1440 requires that exact route's measured active-production margin under one other admitted session. Otherwise proved lower output plus `capability_limit`; document that 1440 goal is unmet on that worker. |
| A · DV-source → SDR, by relevant profile and burn graph | Same separate proof; no substitution of SDR-source or different DV-profile results. Record applicable Profile 5/7/8 behavior and fallbacks. |
| A · HDR-capable panel / preserved HDR output | Record actual panel support first; qualified 1440 preserves grade/metadata, otherwise choose the proved sustainable HDR candidate. Unexpected SDR is a failure. |
| A · Complete cached 2160 vs live 1440 | Compatible complete cache hit wins if network/decode can sustain it and fidelity is equal/better; no new encoder. Test wrong-grade, incomplete, inaccessible and mismatched-track entries are excluded. |
| A · Equal-wire-cost image comparison | 1080 vs 1440 and 1440 vs 2160 at matched total kb/s, fixed codec/grade/audio/fps; original as reference rendered at 2400×1350. Include scope source-rung 3840×1600 and 24/30/60 fps profiles; also test chosen production rates. Retain objective metrics and blinded visual observations. |
| A · Prepared manual changes | Twenty consecutive valid changes per affected platform, existing continuity oracle and full before/after sampling, no A/V/subtitle/position loss; measure tablet dual-decoder load. |
| A · Startup/concurrency/thermal | Same-corpus p50/p95; additional p95 ≤10% or 250 ms, whichever larger. At least 30 minutes at intended admitted load; record pacing-aware production speed, buffer, drops and thermal state if available. No admission bypass. |
| B · Cliff and recovery | A-04 D3 harness, shaping profiles and oracles unchanged. Add high-rung cases using actual measured peak rates and check 1440 is used or skipped appropriately. Frame gaps ≤100 ms on qualifying profiles; any exception requires a separately agreed oracle, not a local weakened threshold. |
| B · Natural-boundary recovery | Twenty-second transient Wi-Fi cliff at minute five of a long title, then a user seek after evidence expiry returns to eligible original in the same owner transaction. Fresh bad-link veto retains reduced route. Measure added seek/resume latency; no claim that every action already reopened playback. |
| B · Mid-play original restoration | Same long-title cliff/recovery with no user discontinuity: bounded candidate establishes source-cost and decode evidence, returns to original without interrupting the foreground. Also prove failed trial cancellation, five-minute backoff, dual-decoder pressure and insufficient admission preserve the encode. |
| B · Paced producer / encode upgrade | Normal 1.0× paced delivery with measured spare active-production capacity may upgrade; saturated or unmeasured capacity does not manufacture proof. Cached/copy route has no encode-speed requirement. |
| B · Full admission / voluntary candidate | Live TV and foreground viewer consume available slots; voluntary upgrade is refused/cancelled without predecessor yield, preemption or fallback reopen. Foreground remains smooth. |
| B · Forced emergency reopen | Existing separate controlled-LAN p95 ≤2.5 s interruption target, with actual coverage. It does not turn a failed D3 zero-restart trace into a pass. |

All numbers are acceptance targets, not results. The A workstream can pass its rows with
explicit per-worker unsupported 1440 tone-map fallbacks, but its report must
state which common tablet source grades still land below 1440. It cannot claim
the HDR-source goal solved by an SDR-source measurement. Workstream B's Apple cliff
parity remains mandatory unless the user explicitly changes that scope.

### 8.3 Review disposition and build prerequisites

The [internal review](DISPLAY-AWARE-AUTO-QUALITY-REVIEW.md) contains both rounds'
dispositions; the [independent Opus review](DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.md)
is preserved as supplied. Opus's verdict was **revise first, narrowly**, not
approval to implement. This revision takes its four P1 and ten P2 findings,
with explicit corrections to the 14 Mb/s arithmetic and to the claim that a
seek/resume necessarily causes an interruption. Product choices are §6.1.

Before each affected slice, close its prerequisites. A implementation need not
wait for B adapter code, but combined promotion waits for every required row:

- **M0-A:** current-main SHA, tablet HDR/decoder facts, create/remote compatibility,
  source-facts path, representative SDR/HDR/DV/burn qualification and cache use.
  Report a failed 1440 tone-map proof before the tablet client milestone.
- **M2-A/M5-A:** chosen rate profiles, equal-wire-cost quality comparison,
  accepted coverage tolerance, scope-rate behavior and physical manual handoff.
- **M0-B/B-R1:** actual nested wire census, fixed-size candidate ID/digest,
  mixed-cluster fixtures and parser-floor rollout/rollback procedure.
- **B3:** A-05 cause/fixture dispositions, measured active-production headroom,
  discontinuity re-plan with fresh-evidence veto, an attainable bounded mid-play
  original restoration proof, and speculative non-preemption.
- **B4:** attainable Apple fresh-transfer measurement, never synthetic parity.


### 8.4 Completion and evidence record

For each case retain source hash/properties, server/client build, encoder and
ffmpeg version, display/container dimensions, settings, network shape, routes,
manifest facts, dropped-frame measurement coverage, rebuffer count, startup and
switch percentiles, final disposition and artifacts. No production credentials
or session bearer URLs in evidence. Add any new document to the docs index in
the same commit.

The combined effort completes only with both workstreams' deterministic
regressions and physical rows, qualified rate policy, explicit HDR fallback,
compatible staged rollout, correct menus and Developer lifecycle disposition,
and reconciled review findings. An A checkpoint is not feature completion.
Full runtime Auto, including proved mid-play original restoration, remains owed
until A-05's assigned work and all platform receipts are complete. Unit tests alone do not
prove that this tablet's decoder or the production GPU can sustain the result.


## 9. Execution ledger — exact sources and outstanding evidence

**2026-09-30 · M0 setup in progress.** GPT-6.1 Sol implementation uses the
managed worktree on `codex/display-aware-auto-quality-m0`. Authoritative
Forgejo `origin/main` was fetched at
`28964229cdb4a70aa0872a49e78fdb096ecf4083` (PR #627). This differs from both
planning snapshots (`ceb7dd8cc` and `b8f3c7587`); neither is used as build
qualification. The user checkout and its untracked planning originals remain
untouched. Four reconciled planning artifacts were copied byte-for-byte and
indexed against this base's own index.

The host has Homebrew Rust 1.98.0 on PATH, which is not qualification.
`rustup run 1.97.1 rustc --version` reports
`rustc 1.97.1 (8bab26f4f 2026-07-14)`; explicit pinned Cargo is used throughout.
`rustup run 1.97.1 cargo check -p plurxd --all-targets` passed on the unmodified
source base in 1 minute 31 seconds. Baseline Clippy also passed with denied warnings in 1 minute 39 seconds; no Rust edits
preceded establishing this loop.

**Re-census at the fetched base:**

| Seam | Actual source finding / consequence |
|---|---|
| Create presentation | `DisplayCaps` in core `playback/caps.rs` is permissive and contains HDR/DV/nits only. No presentation target exists. |
| Quality identity | Core `DesiredQuality` and daemon `QualitySelection` remain `Copy`, strict internally tagged Auto/Original/Manual enums; Auto contains only optional height. No display-quality candidate ID exists. |
| Nested ingress | `ControlRequestV1`, `DynamicCapabilities`, `ClientObservation`, `ClientSelection`, `DesiredSelection` and `MediaIntentEnvelope` remain strict. B-R1 cannot merely relax the outer request. |
| Relays | `ControlRelayRequest`, `ControlResponseV1` and `RemoteStartRequest` remain strict. `RemoteStartRequest.request` carries the existing `SessionRequest`; no new resolved recipe fields may be assumed tolerated. |
| Terminal replay | `RetainedTerminalResponse` itself is permissive, but its nested `ControlResponseV1` is strict. Stored response replay must exercise the whole nested shape. |
| Route ladder | `LADDER_HEIGHTS` remains six lower rungs through 1080. Both starvation and throughput branches of `auto_height_from_prior` reside in `transcode/ladder.rs`; callers are in manager `describe.rs`. |
| Source facts | Manager `plan.rs` already consumes `DecodeFacts`; reuse that path rather than catalog duplication. |
| Ownership | A-05 is unclaimed on this exact main. Its claim must be a draft board PR before runtime surfaces are edited; its existing fixture remains the sole shared ledger. |

**Evidence access, not a result:** Android SDK adb 37.0.1 is installed at the
standard SDK path but absent from PATH. A sandboxed daemon failed; an
appropriately escalated `adb devices -l` succeeded and listed no devices.
The historical TCL 9445X identifier is context, not a current device probe.
Xcode 27.0 is available. Actual TCL model/build/HDR/decoder/dual-decoder facts,
worker SDR/HDR10/DV/burn corpus and concurrent active-production measurements,
Apple fresh-transfer instrumentation and all D3 physical traces remain
`needs:`. No route proof, physical pass, parser rollout receipt or feature
completion is claimed by setup.


### 9.1 B-R1 source contract — parser floor, no feature advertisement

The bounded additions are optional, typed and omitted by every existing
constructor. `deny_unknown_fields` remains in place; required fields,
authorization, generations, owner epochs and closed enum values are unchanged.

| Wire location | Addition / parser-floor behavior |
|---|---|
| `QualitySelection::Auto` and `DesiredQuality::Auto` | `candidate_id`: exactly 32 lowercase hex characters; both enums remain `Copy`. Manual/Original refuse this field. |
| `DynamicCapabilities` | `presentation_target`: signed integer backing-pixel axes and unsigned revision. Axes outside 1..16384 are unknown rectangles, never capability grants. |
| `EffectiveSelection` | Optional `candidate_id`, retained through response validation and terminal JSON replay. Existing owners mint none. |
| `RemoteStartRequest` | Optional `candidate_id` and `presentation_target`, retained exactly. Parser-floor workers refuse candidate-bearing dispatch/takeover; they never silently erase the identity. |
| `ControlRequestV1`, `ControlRelayRequest`, `MediaIntentEnvelope`, `DesiredSelection`, `ControlResponseV1`, `RetainedTerminalResponse` | No direct new fields; their nested typed allowances carry the new shape. Outer strictness is preserved. |
| `ClientObservation`, acknowledgement and action enums | No additions required by this slice. Existing closed vocabularies remain closed. Production headroom can later extend the already-permissive delivery metadata. |

Structural validation permits a tolerant ingress to forward the bounded
identity to its actual owner. Semantic refusal belongs after owner routing,
before any local actor/Store/preparation mutation. It uses the existing
`invalid_control` error with
`selection.quality.candidate_id_unsupported`, so an older error relay still
validates it. Exact terminal replay runs before that refusal; End can clean up
without asking this old owner to dispatch a candidate. Initial create refuses
unsupported candidate intent before recording an accepted ask. No new support
advertisement, selected route, initial fit policy, rate change or controller
ships in B-R1.

The candidate digest domain is the exact UTF-8 bytes
`plurx:auto-quality-candidate:v1` followed by one zero byte, then the full
32-byte canonical route/recipe SHA-256. Its SHA-256 first 16 bytes are the
lookup ID; serialization is lowercase hex. The owner retains the full route
and digest alongside that key and rejects ambiguous/colliding lookups before
checking current authenticated playback, source revision, selected tracks and
worker/client capabilities. A short key alone never authorizes playback.
The canonical route includes delivery method, actual geometry/codec/grade,
source revision and recipe version; presentation revision is excluded because
it does not change bytes. No identity is minted or advertised by this parser
release. Golden tests pin this domain and existing absent-ID digest bytes.

**Regression-discovered correction:** serde's internally tagged unit Original
variant accepted extra fields even with `deny_unknown_fields`. A strict braced
wire variant now deserializes to the same public `Copy` Original value, on both
core and daemon vocabularies. This closes the malformed-candidate hole without
changing a valid Original request or its serialization.

**Source verification:** parser preservation/refusal tests, actual
response relay validation, authenticated internal owner refusal and unchanged
drain/sequence state, durable candidate-bearing terminal replay, public ingress
routing into the remote-peer path, core playback/old-digest tests, pinned
all-target compilation and Clippy. Current focused results are recorded after
final reruns below. Physical measurement and B-R1 deployed fleet receipts
remain outstanding; source compatibility fixtures are not deployment evidence.


### 9.2 B-R1 focused verification — 2026-09-30

All commands below use explicit `rustup run 1.97.1` in the isolated worktree
based on `28964229cdb4a70aa0872a49e78fdb096ecf4083`. This is source evidence,
not a fleet rollout or a physical qualification receipt.

| Command suffix after `rustup run 1.97.1` | Result |
|---|---|
| `cargo test -p plurx-core --lib playback:: --features hiqlite-store` | 87 passed; old canonical/digest golden fixtures remain unchanged. |
| `cargo test -p plurxd --bin plurxd auto_candidate_parser_floor` | 3 passed: nested target/quality/intent relay preservation, malformed/non-Auto extension refusal, remote context retention without dispatch/takeover. |
| `cargo test -p plurxd --bin plurxd a_switch_releases_the_drain_through_the_relay_and_only_when_accepted` | 1 passed; authenticated owner path rejects unsupported candidate and preserves active predecessor, drain and next accepted sequence. |
| `cargo test -p plurxd --bin plurxd settled_rolling_and_vod_routes_replay_the_durable_terminal_ack` | 1 passed; legacy rolling/VOD and candidate-bearing VOD terminal replay. Added public-ingress remote-owner row reaches the peer-transport branch rather than local route refusal. Its unavailable test peer is explicitly not a successful real-cluster exchange. |
| `cargo test -p plurxd --bin plurxd terminal_control_relay_accepts_and_replays_the_exact_ended_response` | 1 passed; actual response validator preserves candidate-bearing terminal result. |

Mac test linking emits an unwind-table size warning; all focused tests above
pass. This is not a denied Rust lint or a playback measurement. Final pinned
workspace all-target Clippy passed. The final adversarial source review found
no actionable blockers, including the internal boxed successor ownership
change; that review ran diff/hash checks, not compiler or playback tests. Both
actor mailbox and dropped-reply ownership regressions passed after that change.
The tracked commit hook, exact-source check and draft PR are being completed. Documentation index: 4 passed; catalog: 27 points,
35 checks, 2572 audited files; staged whitespace clean. Preserved independent
Opus review source artifact (`DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.txt`)
matches the untouched original byte-for-byte, SHA-256
`334766d5e8c591a8b4f91c3da122e34f3df338d55cbf96cd74c5e51859ff246a`.

**Apple M0-B compile experiment from the coordinating session:** a standalone
Swift probe typechecks against iPhoneOS27 and AppleTVOS27 SDKs with the existing
arm64 iOS17/tvOS17 deployment targets. An availability-guarded iOS18+/tvOS18+
`AVMetricHLSMediaSegmentRequestEvent` sequence exposes media-resource request
transaction metrics, network-load/body bytes, response timing, cache and error
facts. This establishes an API compilation path only. No fresh-transfer,
JIT/pacing provenance, foreground impact or physical cliff result is claimed;
older OS and nullable/missing metrics must remain honestly unknown. The full
source/client adapter and shaped-device experiments remain implementation and
evidence work, not a reason to raise the deployment minimum.


### 9.2.1 B-R1 static preflight follow-up — 2026-09-30

The actual gate exposed two static contract omissions: server-first optional
`presentation_target` was compared as if already emitted by native clients,
and two new Axum response-status assertions matched the process-capable
sentinel. The correction keeps exact parity for every legacy field, with a
single explicitly phased optional/omitted target premise that fails when a
client learns the field. The existing nested Rust fixture explicitly verifies
legacy target omission. The inventory delta names both HTTP status reads;
neither creates a task/process or changes producer ownership.

The complete validation-contract suite passed253 tests (one skip), the nested
parser fixtures passed3, and all seven shared web preflight commands passed.
The independent reviewer found no blockers in the exact static correction.
The broader local operations run also exposed the original Opus review's
missing repository Status header. Its supplied bytes are now an indexed `.txt`
source artifact behind the existing `.md` lifecycle wrapper; SHA-256 is unchanged.
The general status audit remains unchanged. Pinned all-target check/Clippy, formatting and history/catalog passed. Existing
documentation index/status checks passed7. An escalated `make operations-check`
ran580 tests with exactly three failures on macOS: the two signing-input tests
`test_a_relative_keystore_is_resolved_before_gradle_sees_it` and
`test_it_refuses_to_start_without_each_signing_input` use `/bin/true`, which is
absent here (`/usr/bin/true` exists); the janitor
`test_a_docker_that_never_answers_costs_one_pass_and_not_the_janitor` requires
GNU `timeout`, absent here, and took60.4s. These unchanged platform fixtures are
not claimed passing. Escalation resolved all seven loopback socket errors and
four pgrep-related failures from the first sandboxed run. The general docs
status audit and unrelated operations fixtures remain unchanged; Linux
preflight is still required.


### 9.3 M1-A source progress — 2026-09-30

B-R1 is committed as `62fcdaa12aa2cffa478dff4aff3b148a069a4885` and reviewed
in [PR657](http://192.168.4.7:3000/noirr/plurx/pulls/657). The tracked hook and
exact-commit check passed. The artifact attachment API rejected this Forgejo
URL; the PR exists. No skipped workflow is treated as qualification. The
combined effort and M1 task retain this reviewed dependency pending ordinary
prerequisite landing; no deployment is claimed.

The first M1 source slice adds exact two-axis fit using upright SAR/rotation
aspect and integer cross-products at the agreed `11/10` scale. It checks actual
even output dimensions; width 2112 admits 1920×1080 while width 2113 refuses it.
Tablet, portrait, scope, SAR, rotation and missing/invalid geometry fixtures
pass. Source coded dimensions remain separate from displayed aspect.

The existing bound `DecodeFacts` FFprobe reader retains measured SAR and
rotation with its existing source identity/lifecycle. A successful stream probe
without a display matrix or rotate tag is upright; malformed/conflicting
rotation or missing SAR stays unknown. Existing artifact FactsDigest remains
unchanged; the normalized candidate recipe in M2 must include the actual SAR,
rotation and transformation version rather than changing legacy cache identity.
Both existing production selective probes now request SAR, rotate tags and
display-matrix rotation. A matrix identified without its rotation remains
unknown; a regression pins the actual incomplete selective-probe shape.
No second probe, catalog migration or unbound fact cache was introduced.

Create capabilities now carry optional render target and nullable per-entry
coded-width/rational-frame-rate limits. Duplicate codec/profile entries remain
separate. Known violations refuse admission; incomplete legacy constraints are
unknown, including zero actual dimensions. Admission of original copy uses
coded dimensions, while normalized encodes use their resulting output geometry.
The new replicated `playback.display_aware_auto` choice defaults off when absent,
is returned by settings/server info, and saves independently of advisory
readiness. Its Developer card identifies combined source/device qualification
and its graduation destination. Both choices and a late-save/newer-draft race
are verified by the existing Settings suite.

Current focused evidence: exact fit2 passed; per-profile admission1 passed;
bound-probe geometry1 passed; Settings35/35 passed; pinned plurxd all-target
check passed. Workspace all-target Clippy passed; the final zero-axis admission rerun passed.
The existing decoder-selection/recipe-identity suite passed all 59 cases,
including source-binding and artifact-identity regressions. Docs index4 passed. This
is source implementation evidence only: worker grade, actual display, physical
recovery, normalized production filters and retained/remote candidate transport
remain later integration/qualification obligations, not passes from pure math.
The final adversarial review found and rechecked the incomplete-matrix P2
correction; its final verdict found no actionable blockers. Corrected focused
probe test and pinned all-target check passed; final Clippy/hook passed.
M1 source commit is `76a883fa0`; the B-R1 follow-up `78e9ba0ee` is retained
on the effort and integrated into this task before exact-tree revalidation.
### 9.2.2 Authoritative-main refresh — 2026-09-30

While PR657's corrected preflight passed, authoritative main advanced from
`28964229cdb4a70aa0872a49e78fdb096ecf4083` to
`f16be4f22296f98a6bce9f2a38b76b2411759e53` (worker Activity/admitted throughput,
cluster probe batching and DVR/Activity contract inventories). This refresh
is fetched-main evidence, distinct from the earlier planning snapshots. The
only merge conflict was the process-shaped sentinel: main407 plus the two
reviewed B-R1 HTTP status reads gives409; both independent explanations remain.

Rechecked source seams: strict playback/control/intent/create/replay boundaries
and decode_facts production probe query are unchanged by this main delta.
Manager construction now integrates bounded source-probe job capacity; retain
that ownership rather than introducing an independent probe/cache lifecycle.
Existing results against28964229 remain historical evidence. Compilation,
focused parser/relay/replay/digest regressions and static contracts were rerun
on this actual merged tree: pinned all-target check passed; core playback87,
parser3, actual owner/refusal1, durable replay/remote-path1 and actual response
relay1 passed; validation253 (one skip) and affected operations85 passed.
No old-base result qualifies it. The sole reviewed conflict was rechecked with
no blockers; final normal hook/commit checks remain recorded separately.

### 9.2.3 Required receive-only runtime decoder snapshot (in progress)

The parser floor also carries optional `decoder_caps` in control capabilities
and remote worker context. Omission retains the legacy/unknown baseline; presence
is a full snapshot, never a patch. An empty `video` array explicitly states no
available decoder. Each entry retains its canonical codec token (including
VP9/decode-only and future unknown tokens), profile tokens, availability, grade
and DV profile claims, and optional width/height, rational frame-rate and bitrate
limits. Unknown codec/profile tokens provide no admission proof. Multiple
entries remain separate; no global minimum collapses their profile constraints.

Structural bounds are 16 video entries, eight profiles per entry, 32-byte
lowercase ASCII/digit/underscore/hyphen tokens, four grades, eight DV profiles
1–10, positive axes through 16384, positive rational components through 1000000
with ratio at most 1000 fps, and bitrate at most 1000000000000 bit/s. Snapshot
and presentation revisions are positive JSON-safe integers through
9007199254740991 so native/JavaScript round trips cannot silently round them.

Revision ordering belongs to the scoped client lifetime at the B-R2 owner:
lower revisions cannot replace newer state, an equal revision with differing
contents is a conflict, and an in-flight successor must be checked against the
latest accepted target/decoder state before commit. This subsection records the
consumption contract; B-R2 enforcement is still pending implementation.

B-R1 preserves the snapshot through structural ingress and relay, then refuses
unsupported nonterminal local dispatch even when `candidate_id` is absent.
Terminal cleanup and retained-response replay retain their earlier ordering.
Remote execution/takeover refuses any snapshot it cannot enforce. Target geometry
alone remains advisory. No client emits these fields yet; the conformance audit
allows exactly the two optional, omitted legacy extensions and fails when a
native client gains either field, requiring restoration of complete parity.
Focused evidence: strict conformance 15/15; complete static validation 253
(one existing skip); docs index 4/4; nested relay/worker parser fixtures 3/3;
actual owner refusal preserving the active session/drain 1/1; snapshot-bearing
public ingress and retained terminal replay 1/1; candidate/revision tests with
`hiqlite-store` 4/4. The final all-target compiler check and workspace Clippy passed on pinned
Rust 1.97.1. Independent schema/relay review found no actionable blockers;
the normal commit hook remains pending. These are source checks, not a parser-floor deployment
receipt or feature qualification.

### 9.4 Dependency integration after parser snapshot qualification

The M1 source retains its reviewed geometry/capability/settings implementation,
with authoritative main f16be4f22 and parser snapshot source 357d47a07 integrated.
The snapshot normal tracked hook passed catalog, pinned formatting/workspace
Clippy and served JavaScript syntax. Parser PR657 remains unlanded; no fleet
receipt or combined-feature promotion is inferred. Exact M1 integration all-target check passed on Rust 1.97.1; core playback
90/90 with `hiqlite-store`, decoder selection 59/59, Developer settings 35/35,
strict protocol conformance 15/15 and docs index 4/4 passed. Normal integration
commit hook remains pending; older M1 results do not substitute for this tree.

### 9.5 M2 normalized pipeline integration (in progress)

Source work now opts new candidate routes into a conditional upright square-pixel
presentation contract; legacy requests, FactsDigest and cache recipes retain
their existing branch. New plan identity carries transformation version, measured
SAR and rotation. Source ceilings use upright square-pixel presentation coordinates:
1440×1080 SAR4:3 retains 1920×1080, while an ordinary 1920×1080 source never
creates a synthetic 1440-high rung. The CPU pipeline, immutable VOD argument
builder and encoded VOD manifest adapter consume the same resolved raster.
Unknown geometry refuses only the new normalization claim. Rotated opaque GPU
graphs are refused until a supported exact transform is proved.

A local FFmpeg9.0.1 synthetic fixture independently measured the display-matrix
sign: +90 matches `transpose=cclock`, -90 matches `transpose=clock`, and180
matches `hflip,vflip`. Manual transforms plus output `rotate=0` metadata retained
the input matrix; clearing the input matrix before manual rotation removed it.
The new conditional argv therefore includes input `-noautorotate` and
`-display_rotation 0`, plus one measured manual transform and explicit `setsar=1`.
Even raster alone does not prove square pixels: a 360×240 SAR64:45 source scaled
to306×144 retained SAR256:255 without `setsar=1`. These local experiments establish
CPU behavior only; every actual eligible worker/build still needs exact graph,
source-grade, fps, concurrency and codec/container proof. No1440 rate profile or
production candidate admission is claimed yet. Core decoder regressions 62/62 passed after a reviewed correction: scalar
rotation is insufficient to exclude reflected/sheared display matrices. Both
production queries now collect full `displaymatrix` and its type; the normalized
branch validates pure unit quarter-turn coefficients and refuses missing,
reflected, translated or sheared matrices without changing legacy routes. Rolling
start/cache now consume the same resolved contract as encoded VOD. New rolling
masters remain attempt-bound; MPEG-TS does not have an AVC init object, so this
is not an init-derived codec claim. Manifest adapter regression/compile checks
are running.

Read-only fleet discovery currently reports FOUR f16be4f22 members: nynuc
192.168.5.236 and m6 192.168.4.14 voters, nuc4 192.168.4.8 leader/voter, and nuc3
192.168.4.7 read-worker/learner. Bonjour's two responders were not a complete
fleet census. Current nynuc UI reports jellyfin-ffmpeg8.1.3, QSV/VAAPI boot graph
validation, but unavailable held FFprobe identity and explicit decode facts;
Main10 plain-HDR and DV-HDR10 graphs failed. These are baseline observations,
not source geometry/tone-map admission proofs or parser-floor deployment receipts.
Missing bound facts must remain unknown through the new adapter.

### 9.6 User-confirmed execution workflow (2026-09-30)

Implementation now uses the independent clone `/private/tmp/plurx-auto-quality`,
with no shared Git metadata or further access to the user's original checkout.
The migration retained all twelve owned source files with exact byte verification
in an external snapshot. Parser prerequisite PR657 landed as
`5c99538fd0f6e96a7a6f6d8772f944735c4f33b1` after its final promotion gate.

The user explicitly replaced intermediate task tests, reviews and effort gates
with batched normal commits, compiler and formatting checks, then one adversarial
pass when the combined PR is ready for main. Findings are addressed before the
fast lane and green merge. Broad unrelated unit-failure cleanup is a separate
process. Physical qualification gaps remain truthful advisory Developer status;
readiness does not reject Save or override the saved feature choice. Safety and
compatibility predicates still describe what evidence actually supports.

### 9.7 Current source integration and compiler receipts

The independent clone passed Rust1.97.1 daemon all-target checks after profile,
manifest and request-context integration. New profile identity is explicit in
TranscodeOptions; it pins H.264 High level5.0, measured normalized cadence and
12Mb/s VBR, with actual audio contributing separately to average/peak bandwidth.
A160kb/s audio budget yields12.16Mb/s average and18.16Mb/s peak. Rolling and VOD
manifest adapters carry both values. Immutable output grid follows the resolved
normalized cadence. Candidate execution context is serde-skipped inside the
legacy SessionRequest; retained outer worker candidate/target/decoder fields
remain the protocol reconstruction seam. Full recipe equality is checked before
VOD admission. Catalog/owner/controller integration remains underway.

Create-time Android, iOS and tvOS sources compile, as do web syntax/types with
the existing baseline unchanged. Target pixels come from the actual player
container. Android decoder maxima come from advertised component/profile limits;
Apple/web absent maxima remain unknown. These are compiler receipts, not
physical smoothness or D3 traces. Existing A-05 ownership is recorded on its
work-board row and combined with this effort.

The packaged bound probe now uses separately pinned upstreamFFmpeg8.1.3 source
SHA2567138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3.
An actual LinuxARM64 Bookworm compile passed; the resulting FFprobe8.1.3 ELF
has no INTERP or DYNAMIC segment (binary SHA256
bdd21afcf90f5cff773affb3912cb522e7a711b60e91dbd23330faedc9468326).
PLURX_BOUND_FFPROBE selects this self-contained parser for held local-file facts;
general scanning retains Jellyfin FFprobe unchanged. The build checks static
closure on each target architecture. x86 compilation and production installation
are not claimed. Receipt files remain under the external task artifact directory
`/private/tmp/plurx-auto-static-probe/compiled/docs`; no production mutation occurred.


### 9.8 Connected route and runtime source batch

The negotiated `route-v1` response carries canonical full-recipe candidates and
an active candidate ID. Decision, manual 1440 create and prepared Auto requests
now resolve through the catalog. Worker envelopes retain candidate/target/decoder
snapshots and reconstruct the internal context before execution; a different
worker recipe is refused rather than relabelled. Final prepared commit compares
the latest target and decoder snapshot with the staged recipe. Explicit candidate
recovery bypasses legacy height reinterpretation after predecessor ownership is
verified. The catalog retains unknown sustainability separately from compatible
manual choices and bounded speculative trials; initial Auto does not treat an
unknown production sample as proof.

An unproved compatible route may obtain a nonpreemptive prepared slot while the
incumbent keeps running. Actual successor frame/buffer/link and live encode
production evidence gates commit. Completed unpaced production measurements are
candidate-specific and require at least two segments and two seconds of active
wall time; permit waits, suspension and retries reset the window. Transfer proof
requires completed network bodies, excludes cache and producer pacing, and expires
at 15 seconds. The six-per-hour voluntary budget excludes severe recovery and
natural viewer discontinuities. Original source peak bitrate remains unknown
unless independently measured; source average is not promoted into peak proof.

For a bounded return to an original with unknown global peak, the clients can
use at least two successor segments covering two seconds of media: completed
unpaced network throughput must exceed 1.8 times the maximum observed segment
wire bitrate. This is empirical evidence for those segments, not a claimed
whole-title peak. Missing media durations or provenance leave that path unknown;
natural-boundary recovery still follows the original-first compatibility rule.

Rust1.97.1 daemon all-target compiler checks passed after the catalog, relay and
commit-fence integration. Web syntax/types and native compilers remain source
checks; new focused regression definitions have not been run under the user's
batched workflow. Client controllers and exact resident-cache offering are now
connected. Cluster catalogs use a separate authenticated, bounded inspection
endpoint; legacy strict placement envelopes remain unchanged. Exact advertised
worker recipes pin create and prepared placement, and takeover reconstructs the
retained candidate before local dispatch. Native Android unit-test compilation
and iOS/tvOS build-for-testing passed without executing tests (versions 140/202).
Final main integration, adversarial review and fast-lane qualification remain
in progress. No physical D3,
TCL playback, source-grade 1440 throughput or production deployment is claimed.

### 9.9 A-05 measured-Link attribution foundation (2026-10-01)

Frozen source `c0914fe7e` implements §3.3's attribution storage boundary:
the measured-Link height/completion pair is independent of legacy supply
negatives, and SQLite v89 / sidecar v11 initialize historical evidence to
unattributed. A private typed proof requires a completed positive body,
known network/cache/pacing provenance and at most 15 seconds of age.
The fold rechecks freshness and keeps completion time independent of receipt.

Five new focused regressions passed once on the precursor working tree;
the subsequent test-only panic-diagnostic repair changed no successful path
or assertion. The committed source passed pinned Rust 1.97.1 compilation
and the normal hook. Sole review 38 approves the bounded foundation.
Actual current-effort composition `7a3d75ee` after #675 retains those source
bytes; final same-record review publication and its gate remain separate.

Actual ClientLog intake cannot establish completed-body provenance and
explicitly supplies no measured-Link proof. A reachable producer and
candidate-aware negative-prior consumer remain required; neither is claimed
by this foundation. Existing controllers and legacy readers remain intact.
Typed recovery, authenticated decoder evidence and physical qualification
remain open; this is not combined-feature completion.

The bounded candidate-reader isolation follow-up refuses legacy starvation
and coarse rate EWMA in actual enabled caps_v2 catalogs and actual HLS Auto
candidate execution, including their `prior_kbps` hints. Legacy routes,
explicit candidate lookup and manual 1440 selection remain unchanged.
This does not yet apply a measured negative to a candidate: the stored pair
has no failed recipe/route identity. Fresh transfer rate proof, candidate-aware
negative selection and the reachable provenance producer remain required.

### 9.10 A-05 ordinary completed-body producer and exact reader (2026-10-01)

The ordinary Decision/Create follow-up binds optional HTTP-only incumbent
receipts to the exact active attachment, authenticated credential/class/network
namespace, serving owner and physical source version. Local authorized complete
unpaced VOD responses issue bounded opaque nonces; accepted EOF precedes async
authority settlement, and proof becomes visible only after settlement succeeds.
One raw claim and one later Link-negative claim share immutable bytes/time and
the original 15-second EOF freshness. Materialized VOD supplies actual immutable
segment duration; rolling bodies without that fact remain Unknown for negatives.
Historical negatives retain their separate TTL; restart or wall-clock rollback
cannot recreate a fresh positive sample.

Web and native completed-body adapters carry this response's nonce/ETag and
server-observed duration. Warm candidate admission requires fresh exact incumbent
Link evidence plus S-10's separately reacquired complete-full-mux cost/artifact
proof. Optional public sidecars are advisory, omitted on durable replay, and do
not change strict worker candidates or original intent fingerprints. Missing
proof does not block ordinary cold/manual/recovery playback or Auto enablement.

Prepared control remains a finite separate follow-up: optional independently
authenticated HTTP observation context must survive accepted actor attachment,
generation and deadline fences; local staged registration follows successful
actual owner prime, not reservation. A trial may use incumbent evidence to start
but must prove its own staged response before positive commit. Capability UUIDs
are not credential/network authority; remote/unknown proof stays Unknown.
Physical 14-versus-20 admission, typed recovery and complete A-05 qualification
remain open; synthetic comparator cases and source builds do not satisfy them.

### 9.11 A-05 local prepared observation (2026-10-01)

The prepared follow-up carries an optional explicit per-call incumbent receipt
in the HTTP header, never control JSON, relay envelopes or intent fingerprints.
Independent ordinary authentication supplies its credential namespace; the
control capability UUID does not. Without trusted-proxy evidence this ingress
uses the actual remote address and ignores forwarding headers. A proxy namespace
mismatch is Unknown, not a control refusal. Every optional query is bounded by
its original exchange or preparation deadline and a short observation budget.

Only an actually accepted local actor exchange can mint a private observational
origin. Same-intent later polls retain that origin; desired-lifetime, owner or
attachment changes invalidate it. The actual stage writer alone mints a separate
stage-lifetime token, invalidated on abort, commit, replacement or retirement.
VOD tombstone and locked registry-removal seams explicitly invalidate both
observational lifetimes, even while an old incarnation snapshot remains held.
Read-only queries neither reserve commit nor renew either lifetime. Registration
follows successful local owner prime; final intake rechecks both tokens, physical
source, serving route and original monotonic EOF after its last awaited read.

The incumbent's fresh Link and independently qualified full-output cost can
support an upward trial, but web/Apple/Android positive stage evidence comes only
from that stage's own completed response, nonce, ETag and server duration. Remote
or absent proof remains Unknown; ordinary manual, recovery and Auto enablement
are unchanged. Real actor/intake regressions and source compilation establish
these bounded contracts, not physical 14-versus-20, decoder, typed-recovery or
complete A-05 acceptance. Formal review and the current effort gate remain
separate from author development evidence.

### 9.12 Native pacing and upgrade observation windows (2026-10-01)

The next bounded A-05 source task uses the existing native film-clock stall
observations and completed-transfer timestamps. Upgrade history belongs to the
installed attachment/attempt: a new item or attempt cannot inherit it, and a
re-read cliff sample cannot renew its original completion time. Native upgrades
require 60 observed seconds from the later of attachment/attempt observation
start and the last actual stall, and 90 seconds since a proved bandwidth cliff,
in addition to the existing headroom/dwell/owner conditions. Missing observation
start is Unknown; the independent 45-second headroom interval cannot replace
the full quiet interval on a fresh attachment.
Qualified headroom accumulates concurrently with these observation windows;
quiet/cliff refusal delays the proposal, not the start of headroom measurement.

Routine producer `held` state is not a stall verdict. Fresh exact-session and
candidate active-production evidence can still attribute saturation while a
producer is held; measured active 2x work behind paced 1x delivery is not
pressure. Missing active timing is Unknown. Successor production, Link cost,
decoder and continuity qualification remain independent. This source task
does not close unknown-cost original trials, natural-boundary integration,
typed recovery disposition or physical A-05 acceptance.

### 9.13 A-05 unknown whole-output source-copy trial (2026-10-01)

The server/web continuation separates private `QualifiedOutput` authority from
`UnknownOriginalTrial`. A trial requires the exact compatible source-copy
recipe, local serving owner, independently authenticated fresh incumbent
receipt and live accepted actor. The current HLS stage resolver implements
that original-video route as `Remux`/`Copy`; unstaged `Original` file delivery
and arbitrary encodes do not acquire this exception. A known peak or known
qualified complete-output cost cannot fall through to the unknown branch.
Exact retained Link negatives still exclude a candidate. Compatible catalog
exposure is distinct from a warm recommendation; a trial never populates
`retained_output` or claims complete-full-mux qualification.

The browser's staged loader binds each completed unpaced network response to
its item, pipeline, stage object, session, candidate and full recipe digest.
Its original trial requires distinct nonce/ETag-bearing segment objects with
server-issued immutable-plan advertised durations, nonoverlapping media intervals, at least two
segments and two seconds of media within the 15-second deadline. Conservative
actual transfer must exceed 1.8 times their largest observed wire cost. That
maximum remains empirical segment evidence, never a whole-title peak or
reusable output sidecar. The qualified complete-output branch keeps its 1.8
margin. Existing incumbent runway/pressure, preparation cancellation,
five-minute failure backoff, presentation continuity and exposure rollback
remain owned by the current controller.

The old `aa0d6382b` browser source rejects the new focused trial regression;
the changed browser source passes it once. The independent captured item/digest
regression and both new authenticated-stage/catalog Rust regressions each pass
once. Pinned Rust 1.97.1 all-target source checking passes. Final committed-tree
checks, native composition, one formal review and the current effort gate
remain pending at this author checkpoint.
This records source work, not physical restoration, D3 or A-05 completion.

**Native interval checkpoint:** Apple cannot infer segment starts from metric
event `mediaTime`. Its stage captures the exact item, player, session and full
recipe, and makes one bounded lookup of that item's actual media-playlist URL.
Only a complete same-origin immutable VOD playlist with canonical unique
`seg%05d.m4s` objects supplies advertised `EXTINF` intervals. The lookup refuses
redirects, ranges, discontinuities, aliases, partial bodies, more than 1 MiB or
8192 entries, and completion after the original observation deadline. Header
duration agrees within outward-millisecond/six-decimal rounding; neither value
claims packet-exact duration. Android retains actual completed-load start/end
intervals and the exact staged pipeline. Duplicate objects, receipts, ETags,
overlap and stale attachment evidence cannot qualify the empirical margin.
Missing proof retains the healthy incumbent without reopening or renewing a
deadline. The same task's native boundary implementation uses the existing
accepted preparation at the final coalesced viewer seek target, including a
backward target, or a resume after an attachment-bound explicit viewer pause
of at least 60 seconds. It never calls ordinary optional create: predecessor
activation can retire the healthy session before the response is returned.
The optional part borrows the original transaction's remaining budget, capped
at eight seconds from its first entry, and reserves two seconds for that same
healthy seek/resume. Refusal, missing proof or supersession cannot create a
second quality operation or renew the fallback deadline. Apple prepares the
exact staged item's native/audio selection before switching. Android intercepts
current MediaSession transport before delegate mutation through a private
forwarding wrapper; a second explicit Pause still revokes an optional resume
while the delegate is held paused, and its SDK consequence is not a new viewer
edge. Stale wrappers cannot issue transport intent for a new attachment.

Boundary admission still requires current authenticated completed-body proof,
decoder/recipe/selection ownership, incumbent runway, exact staged body proof
and presentation continuity. Only ordinary mid-play quiet/cliff eligibility is
not used to pin the viewer boundary after a cliff expires. Ordinary mid-play
45-second headroom, 60-second quiet, 90-second original-EOF cliff, 60-second
evaluation cadence and five-minute failure backoff remain unchanged. Current
native source/test compilation and the two new focused boundary cases are being
qualified before the single coherent review. This remains source work, not
device startup-cost acceptance, shaped-network restoration or A-05 completion.

### 9.14 Durable candidate-recovery storage milestone (2026-10-02 UTC)

The storage slice separates authenticated exact-recipe fault observations from
the one decoder quality-response admission in a playback recovery lifetime.
Current pointer, incarnation, owner, recipe and source-metadata predicates
fence each fold; duplicate observations never refresh the original timestamp
or rearm admission. SQLite92 and Hiqlite68 append after the existing COPY
migrations. SQLite-to-Hiqlite import must retain both the session recovery
epoch and rejected/spent candidate rows; older sources without that epoch
remain explicitly empty rather than receiving an invented lifetime.

The Store API trusts its caller to reconstruct credential, physical source
version and full recipe from authenticated incumbent facts. This milestone
does not yet wire that caller, the native typed-cause senders or sustained
incumbent decode-pressure behavior. Those remain source work, alongside the
original decoder/recovery and shaped-network physical acceptance. It does not
close A-05 or restore the superseded numeric-height controller.

### 9.15 Authenticated recovery consumers and native pressure (2026-10-02 UTC)

The follow-on reconstructs credential generation, physical source version,
full incumbent candidate and recovery lifetime from the authenticated current
route. Private Auto create, Decision and preparation readers consult durable
decoder rejection memory; the public/manual catalog remains unchanged. A
fault observation does not spend the single decoder quality response. Its
actual changed-candidate create claims that response atomically; replay or a
second response cannot rearm it. This leaves further codec recovery with the
existing compatibility owner rather than creating another height controller.

Apple and Android send the closed Link, Encode, Decode, Hold and Authority
cause vocabulary at their existing evidence writers. Hold and Authority are
diagnostic deferrals/terminal observations, not quality failures. Encode needs
fresh actual active producer pressure; Decode needs an actual decoder failure
or sustained dropped-frame intervals while the incumbent advances with at
least ten seconds of supply. Pause, seek, replacement, stagnant progress and
clock rollback discard that pressure window. Legacy `stall` stays legacy.

A native Link recovery awaits an exact nonce acknowledgement within its
original recovery budget, capped at 250 milliseconds, and rechecks the
captured attachment and attempt afterward. The ClientLog intake emits
`X-Plurx-Link-Accepted` only after the authenticated completed-body negative
was actually accepted and, with `playback.network_priors` on, durably folded
(amended by §9.17: with priors off the owner's live receipt backs it). Unknown, positive or duplicate
claims remain ordinary 204 without that header. The immutable acknowledged
nonce accompanies only its matching predecessor/candidate create; a newer
sample cannot substitute for it. No acknowledgement renews the original EOF,
recovery deadline or failure lifetime.

Focused source cases exercise the native acknowledgement predicates and
attachment tickets, real SQLite/route/body claim folding, and authenticated
single-response decoder memory with manual catalog preservation. These are
source proofs, not physical decoder or shaped-network qualification. Current
committed-source checks, one independent review and the exact effort gate
precede landing; the original 14-versus-20 Mbit/s, decoder/recovery and complete
platform acceptance matrix remain open. No saved feature choice is gated.

### 9.16 Viewer boundaries no longer hold Play or a seek (2026-10-04 UTC)

Review finding (P2): after an explicit pause of at least 60 seconds the native
clients did not write Play to the player until the optional original stage of
§9.13 committed at the paused frame or exhausted its borrowed budget — up to
about six seconds of a pressed Play doing nothing — and every viewer seek
waited behind the same attempt. Apple ran that stage as 25 ms `Task.sleep`
polling loops on the main actor.

The boundary rule is now: the viewer action is applied to the incumbent at
once, and the original-first re-plan it is owed is served afterwards by the
ordinary Auto evaluation as an ordinary prepared replacement beside the
playing incumbent (Android's rendezvous, Apple's aligned commit). A seek or a
long-pause resume only arms a one-shot re-plan keyed by the viewer action that
armed it (Android `AutoBoundaryReplan`, keyed by the transport lifetime; Apple
`AutoBoundaryReplan`, fence `autoBoundaryReplanCurrent`), so any newer viewer
action makes it stale without a timer. The first evaluation whose incumbent
is established, playing and has 10 seconds of runway consumes it, and a fresh
link proof still stands in for the mid-play quiet window for that one
preparation. A superseded or refused boundary preparation is withdrawn without
backoff; a failed one is an ordinary Auto failure. The borrowed transaction
budget, the exact-target hold, the optional deadline, Apple's
`AutoViewerBoundaryBudget`, the boundary resume owner and the selection
pre-stage are gone. This changes when the original switch happens (after the
viewer action, through a seamless handoff, rather than instead of it); it does
not change which candidates qualify.

Web had the other half of the same defect and now follows the same rule
(2026-10-04, PR #802). Its long-pause resume ran through `seekTo()` at the
position Play was pressed at; the Auto ask took about a second, so a retained
route still seeked the attached media back by that second. `togglePlay` now
calls `resumeQualityBoundary`, which asks while playback runs and touches the
media only for a picked route. A pause, seek, reattach, or an Auto change
started during the ask supersedes it, and a picked route that fails before
attachment retires its intent without a seek.

### 9.17 Link acknowledgement and upgrade proof without network priors (2026-10-04 UTC)

Defect D6 (main-merge review): every A-05 Link path returned early unless
`playback.network_priors` was `"1"`, and that setting defaults off. With
display-aware Auto on and priors off, the ClientLog intake never claimed a
positive completed body, so a later Link negative had nothing to claim; the
native recovery's 250 ms acknowledgement never arrived and the client reopened
the same quality. The live incumbent proof (`current_positive_until`) refused
too, so no prepared or warm Auto upgrade ever had Link evidence.

**The invariant, amended.** An acknowledgement still needs this exact nonce's
one negative claim, re-proved against the live attachment with its original
EOF, and it is still never refreshed or repeated. What backs that claim now
depends on the setting:

- **Priors on (unchanged):** the negative folds into `candidate_link_priors`
  and only an exact durable readback of the immutable completion acknowledges
  it.
- **Priors off:** nothing durable is written or read. The owner's in-memory
  receipt is the record — `Receipt::claim` marks the nonce negative once and
  `current_negative_until` re-proves it — scoped to the attachment, never to
  the network, and gone with the receipt's 30-second lifetime. The durable
  fold's refusal of an *older* completion for the same recipe has no
  in-memory equivalent across distinct nonces; each nonce is still bounded by
  its own 15-second EOF freshness and acknowledged at most once.

`playback.network_priors` therefore means only what OPERATIONS.md says it
means: whether per-network history (`network_priors`, `candidate_link_priors`)
is kept and consulted. The live receipt is not history, so positive claims,
the fresh-transfer upgrade proof (`current_positive_until`, which reads no
stored prior) and the negative acknowledgement work with it off. Reads of
retained negatives — Auto's `filter_catalog` and the unknown-original trial's
`admissible_for` — stay behind the setting.

**The measured-Link producer.** With priors on, an acknowledged negative also
yields the attributed `MeasuredLinkObservation` §9.9 built storage for: the
incumbent candidate's `target_height` (read from its own published response
and required to be the exact receipt recipe) plus the completed body's server
EOF. The ClientLog task folds it after answering, so the 250 ms wait never
covers that write (`plurx_store_result{operation="observe_measured_link_prior"}`
counts a failed fold). SQLite v93's `link_worst_rung_height` /
`link_starved_at_ms` pair now has a writer; Hiqlite voters fold the same
columns in their node-local telemetry sidecar (sidecar v11 onward), so no
replicated migration was needed.

**Its consumer.** A cold candidate Auto choice (create or Decision, no
incumbent receipt) drops candidates at or above an active
`active_link_starved_rung`, keeping the catalog whole when nothing lies
below. Legacy unattributed starvation stays out of the candidate policy, and
a warm choice keeps using the fresher live transfer.

**Unchanged limits.** Receipts are registered only where the node that
handled the create owns the session, and an IPv6 client has no network
identity; neither is ever acknowledged or proved, and both keep the ordinary
unacknowledged reopen. Physical shaped-network acceptance remains open.

