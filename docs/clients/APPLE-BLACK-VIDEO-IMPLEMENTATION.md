# Apple frame evidence — distinguish an advancing clock from a ready picture

**Status:** implemented; local Apple suites pass; PR review, merge gate, and
physical incident acceptance remain open · **Inspected base:** `eb547f35d` · **Written:** 2026-10-08

This is the build contract for two verified Apple-client detection defects.
Read the evidence limits in §1 before treating any change as a fix for the
reported black picture. Work through the milestones in §6 in order. A change
to Dolby Vision conversion, server packaging, compatibility policy, or the
prepared-switch protocol requires separate evidence and a revised plan.

Companion to [PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md)
(the fault and presentation model) and
[PLAYBACK-LIFECYCLE-COVERAGE.md](../playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md)
(the attachment and recovery boundaries).

## 1. What is established, and what remains unknown

### 1.1 The reports do not identify the rendering failure

The user reports black video with audio for Avatar: Fire and Ash on Apple TV,
both when resuming partway through and when starting over. They also report
black video for a Tom Segura HDR10 HEVC title. The installed Noirr Cinema app
was identified as build 213. Re-verify the physical device's build when taking
new evidence; a source checkout's build number is not installed-build proof.

The retained Avatar delivery is a copied 2160p HEVC Dolby Vision Profile 7 FEL
source, converted after muxing to Profile 8.1. The retained init plus first
segment from the 04:48:31 UTC attempt decodes into 48 actual pixel buffers in a
local macOS AVFoundation experiment. Removing repeated parameter sets also
decodes. The raw pre-conversion Profile 7 clip fails that experiment. These
observations support the converted clip's decodability on that Mac. They do
not establish tvOS HLS decoding, AVPlayerLayer output, HDMI output, or the
cause of the television's black picture.

Read-only inspection found no demonstrated lost layer binding: the ordinary
player view always includes PlayerSurface, normal opens replace the item on
the same AVPlayer, and the layer binds at creation and update. The
`surface_raised` and `surface_cleared` log names describe fault-model changes;
they do not mean that UIKit attached or removed a video layer.

### 1.2 Three code facts justify this bounded repair

Re-verify these anchors at build time because lines may move.

| Code | Verified behavior | Consequence |
|---|---|---|
| `PlayerController.swift`, `ApplePlaybackTTFFState`, about line 661 | TTFF completes after 250 ms of advancing film time while playing | Audio progress can generate a log claiming a first video frame |
| `PlayerController.swift`, `BlackFrameWatchdog.observe`, about line 1693; `sampleSurfacePresentation`, about line 8128 | Positive presentationSize retires the watchdog and counts as picture evidence | Dimensions are accepted as a frame; the detector cannot distinguish known dimensions from absent video output |
| `PlayerController.swift`, `retryMediaOnNextNode`, about line 8879 | An ingress retry replaces the item without resetting the watchdog | A predecessor's latched picture state can disable detection on its successor |

[Apple's presentationSize contract](https://developer.apple.com/documentation/avfoundation/avplayeritem/presentationsize)
describes dimensions. It is not a first-frame acknowledgement.
[Apple's isReadyForDisplay contract](https://developer.apple.com/documentation/avfoundation/avplayerlayer/isreadyfordisplay)
identifies whether the current item's first video frame is ready for display.
Even that stronger signal does not prove an image was visible on the external
television. Name the boundary correctly in code and logs.

### 1.3 Success has two separate receipts

**Code-correctness receipt:** advancing audio plus dimensions cannot certify a
video frame; a replacement cannot inherit its predecessor's evidence; the
existing bounded watchdog uses the corrected evidence when local rendering
is actually expected. Deterministic tests can prove this receipt.

**Incident receipt:** the named installed build shows Avatar and the HDR10
control title on the physical Apple TV through the previously failing actions.
That receipt requires device observations. A successful unit test, Mac decode,
or metadata inspection cannot replace it. If the incident remains unresolved,
the PR and final report must state that explicitly.

## 2. Scope and ownership

| File or surface | Intended change |
|---|---|
| `clients/apple/Sources/PlayerSurface.swift` | Read the current display layer's readiness and visibility without consuming video samples |
| `clients/apple/Sources/PlayerController.swift` | Consume one item-scoped presentation snapshot in TTFF, surface evidence, and the existing black-frame watchdog; reset on all attachments |
| `clients/apple/Tests/AppleClientTests.swift` or a narrowly scoped Apple test file | Deterministic frame-evidence, TTFF, attachment-reset, and visibility regressions |
| Existing Apple diagnostic log structures and tests | Add bounded, credential-free evidence only where current log plumbing can carry it |
| Apple build metadata and release note | Claim the next build using repository tooling; distinguish installed build from candidate build |
| `docs/clients/` plan plus `docs/README.md` | Maintain the implementation and acceptance record together |

Do not add a Developer switch. This is correctness and evidence integration,
not an optional playback feature.

Do not change server ffmpeg arguments, DV conversion, the HLS master, HDR
capabilities, or automatic compatibility ordering. Do not allocate a second
AVPlayerItemVideoOutput or call copyPixelBuffer/hasNewPixelBuffer from the new
sampler. Seek, resume, and prepared-switch monitors already own those reads;
a competing consumer would alter the behavior being diagnosed.

Do not add a second timer, new recovery loop, automatic surface recreation,
or blind forced HDR downgrade. Keep the existing watchdog's six seconds of
observed film progress and its discontinuity protection.

## 3. One synchronous snapshot describes the current local display target

### 3.1 Reuse the bound surface instead of introducing another observer owner

The controller already holds a weak `playbackSurface`. Prefer a synchronous
`@MainActor` query on that surface from the existing periodic observation. The
query takes the expected AVPlayer and AVPlayerItem and returns values; it
neither rebinds layers nor changes playback. A minimal proposed shape is:

```swift
struct LocalVideoEvidence: Equatable {
    let bindingMatches: Bool
    let targetVisible: Bool
    let readyForDisplay: Bool
}

@MainActor
func videoEvidence(
    for expectedPlayer: AVPlayer,
    item expectedItem: AVPlayerItem
) -> LocalVideoEvidence
```

This is a proposed interface, not an existing API. Keep it in an existing
source file unless a separate file improves ownership enough to justify the
project-generation and documentation changes.

`bindingMatches` is true only when the visible `playerLayer.player` is the
expected player and that player's `currentItem` is the expected item. Inspect
the promoted `playerLayer`, never `stagedLayer`. Query and consume the snapshot
in the same main-actor turn without an intervening await. This avoids queued
KVO callbacks carrying obsolete item evidence.

`readyForDisplay` reads the bound layer's `isReadyForDisplay`; mismatched
bindings return false regardless of the layer's old value. Also require the
expected item to be ready to play when accepting positive video evidence.
Use Apple's current-item readiness contract; do not require observing a
false-to-true transition, which can reject a legitimately warm transition.
Do not cache a positive layer value across attachments.

### 3.2 Visibility and readiness answer different questions

`targetVisible` requires an attached foreground-active window, nonempty finite
bounds intersecting the window, and no hidden or zero-alpha ancestor. Include
the active layer's hidden/opacity state and nonempty frame. Share the relevant
checks with `reportPresentationTarget` where practical so two definitions do
not drift. Stage opacity remains irrelevant to the visible target.

A ready offscreen layer is not local presentation. Conversely, a hidden view
is not evidence of decoder failure. Preserve separate booleans in diagnostics
rather than collapsing both cases to `no frame`.

The code should use these meanings:

| Condition | Meaning |
|---|---|
| Binding matches, target visible, item ready, layer ready | A local first video frame is ready on the expected visible layer |
| Binding matches and target visible, but layer unready | A local frame has not been established |
| Binding missing or mismatched | No valid local display observation |
| Background, no window, hidden target, or empty bounds | Local rendering is not presently observable/expected |
| Positive presentationSize alone | Dimensions are known; no frame verdict |

The layer API does not prove continuing frame changes or that the decoded
content itself is nonblack. Do not claim either in telemetry. Correctly
encoded black scenes must be accepted if the layer is ready.

### 3.3 Item identity owns every accumulated state

Replace the watchdog's implicit session lifetime with explicit attachment
identity. Use a weak AVPlayerItem reference compared with `===`, or a monotonic
attachment generation, checked before each observation. A bare retained
ObjectIdentifier is insufficient because an address can be reused after
deallocation. On a different item reset
`lastPositionMs`, `blackMs`, `presentedVideo`, and `fired`. On no item clear the
binding. Reusing the same item does not reset its state on every periodic tick.

A single production attachment hook may be used instead, but it must be
called from every replacement/adoption path and covered by integration tests.
The self-checking identity comparison at observation is preferred because a
future replacement path cannot silently omit the reset.

The first sample on a new attachment establishes a baseline; time between
old and new items never adds to the six-second budget. A queued recovery
still uses the existing attempt and exact-item guards before taking action.

## 4. Consumers use the same evidence without changing recovery ownership

### 4.1 TTFF distinguishes video readiness from audio progress

Keep the monotonic open timestamp, existing rebase behavior, and one-shot
semantics. Replace the shared assumption that 250 ms of clock advance proves
any kind of frame.

| Playback | Completion evidence |
|---|---|
| Video with a visible local target | Current-item layer readiness from §3; no dimensions or audio-only substitute |
| Confirmed audio-only source | Existing playing plus 250 ms of media-clock advancement |
| Video on external playback or active PiP | Do not manufacture a local first-frame measurement from route activity or audio progress |
| No known source kind or no valid target | No video TTFF result yet |

For video, measure the first **observation** of readiness. The existing
periodic callback may stop while paused; readiness that arrives after the
last paused callback is reported at the next callback, not at an invented
instant. Do not add a new observation owner to make a stronger timing claim.
Preserve the open anchor and item binding when a seek rebases position before
completion. While the existing seek owner has pending presentation, do not
complete TTFF from an old-position ready layer. Complete only after that
owner settles, without adding a competing pixel-buffer consumer.

Keep the existing `ttff` event if compatibility requires it, but make its
message and optional evidence field accurate: `video-frame-ready` or
`audio-progress`. Do not call the audio result a first frame. Do not describe
layer readiness as physical presentation. If adding evidence fields requires
an unrelated server/schema expansion, first use the established diagnostic
detail path and explicitly record that serialization choice in the PR.

### 4.2 Surface evidence requires the current target

Replace the presentationSize and predecessor-latch terms in
`sampleSurfacePresentation` with the current local video evidence. The
existing moving-clock/rate checks still decide whether the session is
presenting. Audio-only playback keeps its existing progress rule.

Do not reuse `surfaceHasPresented` as current-item frame evidence. It records
historical session context and may continue to serve that purpose; it cannot
prove that a new item is ready.

PiP/AirPlay retain their existing explicit-route treatment for the surface
model. Label that observation as externally declared and exclude it from the
local-frame and local-decoder-failure claims. Active routing is not physical
frame proof.

### 4.3 The existing watchdog only judges observable local output

Feed the watchdog the current attachment, a positive current-frame-ready
signal, and whether a visible local target is expected. Retire the watchdog
only for a current-item positive readiness observation, never dimensions.

Accumulate no failure time for audio-only content, pause, background,
missing/hidden local surface, or external/PiP playback. Reset the continuity
baseline when local rendering is not observable, so returning to foreground
does not count unseen elapsed time as a black screen. A concrete decoder
failure requires the same local attachment and visible target while the
film clock progresses for the existing six-second threshold.

Keep discontinuities greater than the existing two-second sample ceiling
from adding time. The existing recovery fences remain. Immediately before spending decoder
failure evidence or entering fallback, recheck the complete current snapshot:
item/player binding, foreground and visible target, local rather than
PiP/AirPlay routing, playing state, and absent readiness. Repeat after any
genuine suspension before fallback. Late readiness cancels the trigger.
If eligibility disappears, cancel the queued trigger and rearm the same
unready item with a fresh continuity baseline when it becomes observable
again; cancelling must not leave `fired` latched forever.

Do not change `handleBlackFrameDecodeFailure`'s established compatibility
ladder ordering in this work. The corrected detector may reach that ladder
in cases the old detector incorrectly suppressed; this is a behavior change
and must have a `fix(` subject and a named regression.

### 4.4 Prepared promotion and rollback keep their existing proof

The prepared pipeline already checks staged-layer readiness before promotion
and consumes aligned pixel buffers for its own handoff protocol. Keep those
checks and their sample ownership intact.

After promotion, the shared snapshot reads the newly promoted `playerLayer`
and matches the adopted successor's exact item. Old watchdog state resets
because the item changed. A warm-ready successor may establish readiness
immediately; do not demand a fresh KVO transition.

After rollback, the same rule applies to the restored incumbent. Any queued
failure for the discarded successor fails the item/attempt guard. The
staged predecessor never supplies local first-frame evidence for the active
successor. `dismantleUIView` must still clear only its own binding; an old
view's teardown cannot clear the controller's newer surface.

## 5. Diagnostics distinguish absence of evidence from a decoder verdict

Add a compact snapshot to the existing opt-in playback diagnostic event or a
bounded transition event. Prefer current diagnostic plumbing over logging at
every periodic sample. The snapshot should include source kind, playback
attempt, binding-match, target-visible, item-status, layer-ready, dimensions,
transport-playing, external/PiP state, and watchdog accumulated milliseconds.
Use opaque per-process attachment sequence numbers if correlation needs them;
do not log object addresses, media URLs, capability tokens, or credentials.

**How to read it:** audio progressing + dimensions positive + visible matching
layer unready confirms the formerly hidden detector condition. A missing or
hidden target instead points to surface/lifecycle investigation. A ready
matching visible layer with a physically black TV leaves image content,
HDR/output negotiation, and the final display boundary unresolved. A fresh
pixel buffer observed by an existing owner may be logged separately, but the
new sampler must not consume one to fill that field.

For the first diagnostic build, keep evidence changes reviewable separately
from any new fallback activation if the team needs physical observations to
settle visibility semantics. Do not silently describe a diagnostics-only
build as repairing the viewer's black video.

## 6. Build milestones and acceptance

### 6.1 Freeze the contract and review it adversarially

Review this plan for false positives during backgrounding, PiP/AirPlay,
warm promotion, item replacement, and late readiness. Confirm that every
claim distinguishes dimensions, layer readiness, and physical display.
Record each finding and its disposition before writing runtime changes.

**Acceptance:** the review has no unresolved blocker to the chosen bounded
implementation; the original black-output cause is still marked unproven.

### 6.2 Implement snapshots and truthful evidence

Implement the main-actor snapshot and deterministic evidence policy. Integrate
TTFF and the surface sampler, with attachment identity and accurate logging.
Do not consume pixel buffers. Preserve the existing transport and selection
owners. Add diagnostics sufficient to separate visibility from unready video.

**Acceptance:** focused tests below pass on iOS and tvOS; both applications
compile. The dimensions-only test fails against the old implementation and
passes against the new one.

### 6.3 Repair watchdog attachment and eligibility

Make every attachment self-resetting, route the readiness snapshot into the
existing watchdog, and fence the queued recovery against a frame that became
ready after its trigger. Preserve the established six-second/two-second
bounds and fallback ordering.

**Acceptance:** a dimensions-known/no-frame fixture fires exactly once when
locally visible, while hidden/external cases do not spend the decoder ladder.
An ingress replacement cannot inherit the predecessor's positive evidence.

### 6.4 Qualify what can be proved and retain what cannot

Run the focused regressions first, then the affected Apple suites and required
repository checks. Fix failing unit tests according to the new contract;
never replace them with skips or make an unreachable network URL pretend to
be a ready AVPlayerLayer. Have a fresh adversarial agent review the PR diff,
including all test changes, and address its findings.

**Acceptance:** the current reviewed candidate passes the repository's
applicable merge gate; its regression fields and validation receipt name the
actual tested tree. Physical incident acceptance is recorded separately.

## 7. Tests exercise decisions, not fabricated AVFoundation readiness

Use plain value snapshots for readiness policy tests and real unready objects
only to verify binding/refusal. Headless tests cannot force
AVPlayerLayer.isReadyForDisplay=true. Do not swizzle that property or assert
that a URL with no server produces frames. A narrow injected snapshot reader
is appropriate for controller integration, defaulting to the real reader in
production. Keep existing debug-only seams debug-only.

| Proposed test | Required assertion |
|---|---|
| `testVideoTTFFRejectsClockAndDimensionsWithoutLayerReadiness` | Playing audio, advancing clock, and 3840×2160 dimensions do not finish video TTFF |
| `testVideoTTFFCompletesOnceForCurrentVisibleReadyLayer` | Matching ready video completes once; repeated observations do not log again |
| `testAudioTTFFUsesProgressWithoutVideoLayer` | Confirmed audio-only media retains its 250 ms progress behavior and truthful evidence label |
| `testFrameEvidenceRejectsWrongPlayerItemAndStagedLayer` | Wrong player/item or staged-only readiness cannot establish the active picture |
| `testBlackFrameWatchdogRearmsForIngressReplacement` | A ready predecessor followed by a different unready item accumulates its own budget and fires once |
| `testBlackFrameWatchdogIgnoresHiddenAndExternalTargets` | Background/hidden/no-window/PiP/AirPlay time cannot become a decoder failure |
| `testBlackFrameWatchdogDoesNotCountAttachmentOrSeekDiscontinuity` | Replacement and large clock jumps establish a new baseline |
| `testQueuedBlackFrameRecoveryRechecksEligibilityAndRearms` | Queued trigger then external/hidden target does not spend recovery; returning visible can detect failure again |
| `testPendingSeekDefersFirstFrameReadiness` | An old-position ready layer cannot finish a pending TTFF measurement until the existing seek presentation owner settles |
| `testLateReadyFrameCancelsQueuedBlackFrameRecovery` | A frame that becomes ready before the queued task acts prevents fallback |
| `testPromotionAndRollbackUseCurrentItemEvidence` | Simulated identity transitions accept warm-current readiness and reject stale successor callbacks/actions |
| `testSurfacePresentationDoesNotUseDimensionsAsPictureProof` | Known dimensions and advancing audio cannot clear a video fault by claiming a current picture |
| `testOldSurfaceDetachDoesNotClearReplacementSurface` | Real object identity checks preserve the newer weak surface owner |

These are proposed test names. Use the final executable names in the PR's
`Regression-Test:` lines. Update the existing TTFF rebase/one-shot and black
watchdog tests; preserve their meaningful pause, discontinuity, exhaustion,
and timestamp assertions. Update the source-contract assertion near
AppleClientTests.swift:4132 which currently requires the invalid predecessor
watchdog latch: assert the replacement production evidence wiring and retain
its single-owner presentation checks. Pure-policy tests do not qualify HDMI
output.

## 8. Commands, integration, and merge evidence

Re-verify the generated test target names and available simulator destinations
before copying `-only-testing` selectors. The current project uses module
`plurx`, schemes `plurx-iOS` and `plurx-tvOS`, and `AppleClientTests` test class.
Use the repository's Makefile as the source of destination defaults.

```bash
make apple-build-bump       # Claim the next build; do not assume 214 is free.
make apple-build            # Compile both Apple platforms without signing.
make apple-test             # Run the affected Apple suites on both platforms.
make operations-check       # Check documentation, build, and static contracts.
git diff --check            # Reject whitespace damage in the final patch.
```

For the focused loop, generate with `xcodegen generate` under
`clients/apple`, use `xcodebuild ... -scheme plurx-tvOS ... test` with
`-only-testing:<verified-test-target>/AppleClientTests/<test-name>`, and repeat
for iOS. Record the fully resolved commands and destinations in the PR rather
than publishing an unexecuted placeholder as evidence. Reuse DerivedData.

No Rust change is needed for the preferred implementation. If investigation
requires Rust edits, establish the repository-pinned Rust 1.97.1 compiler
loop before editing; do not use CI as the compiler.

This is one bounded Apple correctness change, so one ordinary branch and PR
into main is appropriate. If split into multiple overlapping implementation
tasks, follow the repository's effort-branch convention. Commit normally with
the tracked hook, use a `fix(` subject for behavior changes, and include one
`Regression-Test: <path>::<test name>` line per corrective regression. Carry
the same lines into the landing commit.

After any base movement, rerun validation for the exact intended candidate.
Use the applicable main-target gate documented in `DEVELOPMENT_PIPELINE.md`;
if promotion qualification is required, retain its receipt before merge.
The plan does not authorize treating an earlier tree's green run as current.

## 9. Physical receipt and rollback

On the actual Apple TV, record installed build, tvOS version, output mode,
media identity, request position, playback attempt, and observed picture.
Exercise Avatar cold start and the retained resume region around 6521 s, the
reported HDR10 Tom Segura title, and a known-playing SDR control. Observe
controls over the image to distinguish application rendering from an entire
HDMI blackout. Avoid changing multiple delivery variables in the same trial.

For evidence integration, verify that the recorded target matches the active
layer and item and that readiness changes agree with observed local playback.
Exercise an item replacement, background/foreground, and a prepared change or
rollback when available. On supported iOS hardware, cover PiP/AirPlay without
counting route transitions as decoder failures.

If the new diagnostic snapshot reports ready video while the TV stays black,
record that result and continue the original incident investigation at the
output/content boundary. If it reports an unready visible local layer,
verify the existing bounded recovery's outcome and whether the newly
selected delivery actually renders. Neither observation alone establishes a
server DV defect, especially while an HDR10 source reproduces the symptom.

Rollback is a normal revert of the bounded Apple changes and a new build
number through the standard delivery process. Do not revert server media
conversion or mutate source files as part of this rollback. Retain diagnostic
receipts and label superseded build evidence so the investigation remains
reproducible.

## 10. Adversarial plan review — findings addressed before implementation

The independent review on 2026-10-08 requested three corrections. R1 (P1):
recheck complete eligibility before queued recovery spends evidence, and
rearm after an ineligible cancellation; incorporated in §4.3 and its
controller-level regression. R2 (P2): periodic media-time callbacks cannot
promise immediate paused readiness; §4.1 now measures first observation and
explicitly permits deferral until the next callback. R3 (P2): same-item seek
readiness can belong to the old position; §4.1 now defers completion while
the existing seek presentation owner is pending. The review's identity
reuse and stale source-contract test requirements are incorporated in §3.3
and §7. The review did not establish the original physical cause.


## 11. Retained investigation and implementation receipt

The separate temporary diagnostic uses the same physical Apple TV as the
report (tvOS 27.0). The user confirmed both the generated H.264 pattern and
captured Avatar scene were visible as local MP4. With identical captured
Avatar bytes, standalone HLS media playlists produced ready layers and real
pixel buffers; multivariant PQ playlists enabled the video track and advanced
audio, but the layer stayed unready and produced zero pixel buffers. The
problem reproduced outside Noirr Cinema's player surface.

| Controlled experiment | Observation |
|---|---|
| Exact High-tier codec and PQ/DV supplemental master | Black video with advancing audio; known 3840×2160 dimensions |
| Main-tier hvcC, then matching in-band VPS/SPS tier changes | No restored frame |
| No codec attributes, retaining PQ | No restored frame |
| No PQ declaration on the original stream | Explicit incompatible-asset error; not a valid repair |
| No video output tap, nil attributes, explicit 10-bit, BGRA | No restored frame in any master case |
| Separate audio/video renditions; video-only master | No restored frame |
| Remove repeated parameter sets; strip DV to HDR10 | No restored frame |
| Fresh 4K HEVC encode with regular sample timing | No restored frame through PQ master |
| Apply loaded display criteria before attaching item | No restored frame |
| Native AVPlayerViewController and production audio-session category | No restored frame |
| Generated 720p Main10 PQ master | Ready layer and decoded pixel buffers |

These experiments establish a failing 4K HEVC HLS multivariant path on this
Apple TV, not its internal AVFoundation cause. They do not justify claiming
that Apple universally rejects High tier, removing native subtitle groups,
rewriting source media, or changing HDR policy. Current HDR eligibility was
true, HDR modes included HDR10 and Dolby Vision, and enabled video/audio
tracks produced no item error. On installed build 213, the user subsequently
restored a picture by choosing 1080p and then **Apply with restart**. Server
receipts identify the working stream as AVC 1920×1080 SDR (BT.709), AAC 5.1,
VOD. The failed stream was HEVC 2160p DV with E-AC-3. Multiple axes changed,
so this does not isolate resolution as the cause. The preceding prepared
change was rejected because the audio codec/channel shape changed and
required a reopen; the restart action correctly recovered it. The candidate
was built as 215 but deliberately not installed during the user’s movie.

The Tom source is HDR10 Main10 Main tier level 5.0 and begins with an IDR.
Thus Avatar's High tier, Dolby Vision conversion, and CRA resume opening do
not explain both reports. A short Tom copy was obtained only after explicit
user approval. Diagnostic media and raw logs remain outside the repository.

The application defect is independently proven: dimensions and audio progress
prematurely establish video and retire the only detector able to advance the
existing compatibility ladder for this failure. The implementation repairs
that detector, first-output reporting, and per-item lifetime. It does not
claim to repair Apple's decoder or certify physical recovery before the
candidate has been observed.

Local validation on the implementation: 19 focused tvOS cases; complete tvOS
suite 848 cases; complete iOS suite 865 cases; playback surface operation fence
25 cases; Release compilation for both platforms; no failures. The first tvOS
runner completed all cases but stalled writing its result bundle; the serial
rerun completed successfully. PR evidence records the resolved commands and
current candidate. The final merge gate and installed-build acceptance remain
mandatory separate receipts.
