# Playback information — Sol build handoff for dimensions and aspect

**Status:** building; follow [the execution status page](PLAYBACK-INFO-DIMENSIONS-STATUS.md) for actual progress and evidence.
**Written / revised:** 2026-09-25. **Evidence base:** `origin/main` at
`38f61dfe6`, inspected with `git show` without changing the dirty checkout;
refresh before implementation. **Scope:** web, Apple and Android Live TV,
plus truthful labels in their shared VOD panels.

This is the single, self-contained build handoff for Sol. It contains the
current behavior, chosen UI, exact evidence rules, file ownership, build
sequence, test cases and completion requirements. No separate proposal or
review document needs to be pasted with it. Source links are navigation aids.

**Instruction to Sol:** implement this repair, package by package, on a fresh
isolated integration branch. Execute through tested commits and review-ready
PRs; follow the task's explicit merge/release scope and repository gates for
promotion. Do not deploy or re-land unrelated work merely because this
handoff mentions it. Do not restart product design or request another design
review for the decisions settled here. Resolve routine implementation choices
within the contracts below and record material deviations.

Fable reviewed the earlier plan. All B1–B4 and N1–N7 corrections are included;
§11 preserves the disposition. No second design review is claimed or required
by this handoff. The repository's required review of the implemented PR is a
separate obligation. Product implementation has not begun in this document-
writing task.

**Reading and execution order:** §0 starts the checkout; §§1–3 explain the
chosen behavior; §§5–7 define the invariants; §8 is the package sequence;
§12 supplies concrete build recipes and commands; §§9 and 13 define proof
and completion. Keep the execution ledger in §10 current in each package.

## 0. Start Sol's build without disturbing the authoring checkout

1. Inspect the provided repository and its actual remotes. Fetch the current
   integration base; do not assume the author's old HEAD is suitable.
2. Resume an existing effort/worktree only after checking its ownership and
   uncommitted changes. Otherwise create one isolated effort checkout.
3. Bring in this document and screenshots only if absent from the fresh
   base. Apply the older output-facts handoff ruling from §7 as a targeted
   edit, not by overwriting that whole file from the dirty authoring tree.
4. Read current contributor and pipeline rules on the refreshed base. Record
   changed policy, compiler/runtime versions and the exact base commit.

Example commands from the repository root; choose an unused workspace path
and stop to inspect an existing branch/worktree instead of replacing it:

```bash
git status --short                              # Inventory; do not reset or stash.
git remote -v                                   # Identify the intended repository.
git fetch origin main                           # Refresh the integration base.
git worktree list                               # Inspect existing checkouts.
git branch --list effort/playback-info-dimensions

# Only when that effort does not already exist:
git worktree add -b effort/playback-info-dimensions \
  /private/tmp/plurx-playback-info-dimensions origin/main
cd /private/tmp/plurx-playback-info-dimensions
git rev-parse HEAD                               # Record the actual source base.
node --version
python3 --version
xcodebuild -version
xcrun simctl list devices available              # Record usable iOS/tvOS destinations.
```

The authoring checkout at `/Users/pjunod/code/plurx` contains unrelated Rust,
Android and documentation changes. Transfer no lockfile, Android back-button
work, credentials or repository-wide diff from it. No Rust change is planned.
If one becomes necessary, establish and verify the pinned 1.97.1 compiler
loop before editing Rust; an unpinned toolchain is not equivalent evidence.
Use the repository's source-only cloud loop when needed.

Create `codex/playback-info-dimensions-m0` from the effort for M0; after its
validated PR lands into the effort, branch M1 from the updated effort and
continue sequentially through M4. Never branch all packages from the original
base: the shared fixture and builders intentionally overlap. Commit through
the tracked hook. Do not use CI to discover compile failures.

## 1. The problem and the decision

The screenshot compares **Playing resolution: 853×480** with **Broadcast
source: 704×480i**. The first is a browser presentation measurement; the
second is a stored video frame. Their unequal widths imply an unexplained
resize even though the difference can be ordinary pixel-aspect correction.

The server also supplies conversion reasons which the web Live TV adapter
currently drops, leaving the panel to say no further reason was reported.

**Build decision:** compare broadcast and stream **frame dimensions** directly,
show **player display dimensions** separately, label planned versus measured
output, and display the actual conversion reasons. Keep the existing four
modes and playback behavior. No encoder, scaler, tuner, deinterlacing,
packaging or playback-route changes belong in this repair.

**Settled decisions:** use the frame-first composition; accept planned
or unavailable output on web/Apple as an honest completed result; use
Android's existing decoded-frame measurement; give each field one label
across clients and modes; and keep new output probing outside scope.

## 2. How broadcast resolution and aspect work today

This is a current-code summary, not a fleet deployment claim or a probe of
Paul's actual Cozi session.

| Measurement | Meaning | Example |
|---|---|---|
| Frame dimensions | Stored video pixel grid | 704×480 |
| Scan type | Interlaced or progressive | 480i describes an interlaced source |
| Pixel/sample aspect ratio (SAR) | Width of one pixel relative to its height | 40:33 |
| Display aspect ratio (DAR) | Intended picture shape after pixel correction | 16:9 |
| Player display dimensions | Presentation size reported by the player | 853×480 in a browser |
| Player bounds | Layout space in the page or app | Depends on window and device |

For uncropped, unrotated video, DAR = width / height × SAR. A 704×480
frame with 40:33 pixels has a 16:9 shape; at display height 480 its square-pixel
equivalent width is about 853.33. With SAR 10:11, the same 704×480 grid is
4:3 and displays at 640×480. The stored dimensions alone establish neither.

The screenshot is consistent with the 16:9 example, but its actual source
SAR and encoded output have not been measured. No rendering or fixture in
this document proves that session preserved its aspect or frame size.

**Server observation and planning.**
[Source probing](../../crates/plurxd/src/live_tv.rs) inspects a bounded local
prefix from the tuner feed, recording dimensions, field order, rational
frame rate, SAR, codecs, color and audio facts. The same retained bytes are
replayed into the producer. The
[delivery resolver](../../crates/plurxd/src/live_tv_delivery.rs) combines
source facts with client codec/packaging, size, frame-rate and interlace
claims. It resolves video and audio separately and records reasons.

| Condition | Current behavior |
|---|---|
| Compatible original video and audio | Copy compressed tracks and repackage for HLS; no video filter. |
| Compatible video, incompatible audio | Preserve video where the final packaging tuple permits; convert audio as needed. |
| Unsupported video route | Encode H.264, retaining source dimensions when limits allow. |
| Interlacing unsupported by client | Server deinterlaces and encodes. |
| Source exceeds an explicit/client dimension limit | Plan a smaller frame; ceilings do not request upscaling. |
| No supported route or processing unavailable | Fail with the appropriate route/capacity error. |

**Original / Auto.** Modern starts carry client capabilities. Their saved
maximum height defaults to 0 (no explicit ceiling), with optional
480/720/1080/2160 ceilings. Legacy starts without capabilities use the
separate old output-height setting, default 720, as a conservative ceiling.
Even this path clamps height to the source. Encoded dimensions must fit one
complete client limit; they are selected from stored pixels, not browser
presentation size or screen bounds.

**Encoding and aspect.** The Live TV filter adds `bwdif` for deinterlacing,
`scale=-2:<height>` only when reducing height, and encoder-required pixel
format/upload filters. It does not force square pixels, 16:9, cropping or
padding. Aspect handling relies on source metadata and FFmpeg's filter,
encoder and muxer behavior. The scale filter can adjust SAR to preserve DAR.
The planner rounds its downscaled width down to even, while FFmpeg computes
actual width; a planned width is therefore not an output measurement.

Deinterlacing defaults to one progressive frame per source field: nominal
29.97 interlaced frames/s become 59.94 progressive frames/s. The alternative
frame mode retains one output frame per source frame. Neither inherently
changes spatial height. The web envelope advertises `interlaced:false`, so
web Live TV takes the server deinterlacing route for interlaced sources.
Android TV now advertises hardware MPEG-2 support and sink deinterlacing
when available. A supported ATSC 1.0 interlaced stream can therefore be
copied (`video_action=copy`, `deinterlace=false`, source `field_order=tt`).
This differs from web/Apple/handset routes that do not claim that support;
do not assume every interlaced broadcast is converted by the server.

**Clients and information collectors.**

| Client | Picture rendering | Live TV info measurement |
|---|---|---|
| Web | CSS `object-fit:contain` fits intrinsic picture in its slot. | `videoWidth`/`videoHeight`: may include pixel-aspect correction. |
| Apple | `AVPlayerLayer.videoGravity = .resizeAspect`. | `AVPlayerItem.presentationSize`, converted to integers. |
| Android | Media3 `PlayerView`, with no custom resize-mode override in this path. | `videoSize.width`/`.height`, without multiplying the displayed number by `pixelWidthHeightRatio`. |

A 16:9 layout slot does not establish that the content itself is 16:9.
Fitting may add bars; bars may also be encoded inside the broadcast. This
path does not detect or crop programme bars.

Today the web Overview combines browser display size with channel-derived
source metadata; Details includes planned stream format. Those observations
can also have different ages. The output descriptor has no measured output
SAR, and the panel does not verify encoded aspect preservation. The repair
must expose these limits rather than replace the mismatch with false certainty.

## 3. Build these panels and visible behaviors

Keep Compact, Overview, Details and Diagnostics, including their existing
stored mode values. In Overview, put comparable frame facts first, with
provenance immediately beneath each. Stack at narrow widths. Preserve sound,
buffering, reception and the other existing sections.

**Overview — illustrative SD example, using planned output.**

```text
Playback info                       Playing · Live TV

Source frame                        Stream frame
704×480                             704×480
MPEG-2 · Interlaced                  H.264 · Progressive planned
Source probe                        Planned output

Player display size                 853×480
Aspect-corrected size reported by the player.
The broadcast uses non-square pixels. The player's display size
is consistent with its 16:9 shape.

Video converted · no resize planned
The active player did not claim the complete source video route. ·
The player did not claim a usable deinterlacing path for this source.

Original audio                      Subtitles
AC-3 · 2 channels                    Off
Track metadata; device output is not reported.
```

The aspect explanation requires evidence; the example assumes SAR 40:33
and compatible presentation semantics. Without those, retain the facts and
say `Broadcast aspect not reported` or withhold the comparison. Do not infer
SAR or conversion reasons from this illustrative panel.

**Details — the same example, with evidence visible.**

```text
Picture
Source frame             704×480       Source probe
Source pixel aspect      40:33
Source display aspect    16:9          Derived from source frame and SAR
Stream frame             704×480       Planned output
Stream pixel aspect      Not measured
Player display size      853×480       Browser intrinsic dimensions
Broadcast format         MPEG-2 · Interlaced
Stream format            H.264 · Progressive · 59.94 frames/s (planned)
Broadcast cadence        29.97 frames/s · 59.94 fields/s
```

**Real reduction:** a measured 1920×1080 → 1280×720 comparison says `Stream
resolution reduced`. A plan-only comparison says `Resolution reduction
planned`. Neither says merely `Video converted` and leaves the resize hidden.

**Missing output:** show `Stream frame: Unavailable`, even when player display
size exists. A known height alone reads `Height 480`. No source-width fallback.

**Compact:** use the fixture label `Player display size` unchanged on
web/Apple. `Stream frame` is a separate field available in all four modes,
including a measured Android value with a `Measured stream` note. The
Android display field is unavailable with the explanation `Not reported by
this player`; it must never contain sample dimensions. Compact has four
fields on every client: Player display size, Stream frame, Playback and
Buffered on device, with the existing fixture labels for the latter two.
There is no `compact_label`, per-platform label override or new per-mode
availability mechanism in this repair.

**Diagnostics:** expose plan versus observation, reason codes, source/sample
provenance, attachment identity and genuine timestamps where available. Missing
measurements stay missing. A source SAR must never fill the output SAR slot.

**Conversion wording:** use supplied reasons, rendered as text. A generic
`video_incompatible` does not prove MPEG-2 alone was the cause. Equal frame
sizes do not mean original quality, lossless conversion or preserved aspect.
If reasons are absent, say `The server did not provide a conversion reason.`

**Rendered previews.** The text above is the retained illustrative design.
The source attachment referenced prototype images outside the isolated clone;
they are not production playback evidence. The prototype's four modes and
four example states passed browser checks at
780, 390 and 320 px widths, without page errors or horizontal root overflow.
These are prototype checks, not production playback evidence.

**Implementation boundary.** Existing trustworthy collectors may supply
measured facts. Planned/unavailable output remains a completed, honest result
where measurements do not exist. New output probing, decoder instrumentation
or media-session APIs are outside this repair. Reconcile with any output-facts
work already on the refreshed base rather than duplicating it.

## 4. Establish an isolated integration base and explicit ownership

The authoring checkout contains unrelated changes, including a modified
lockfile and docs index. Do not reset, stash or copy the entire checkout.
Start an isolated checkout from current main; import only this document, its screenshots and its index row.

Use `effort/playback-info-dimensions` for integration. Each task branch is
`codex/playback-info-dimensions-<package>` from the current effort, with its
PR targeting that effort. There is no disjoint-file exception: several
packages share the field contract and renderers. These package names describe
sequential work; they do not request parallel agents.

| Package | Primary ownership | Shared edits allowed |
|---|---|---|
| M0: atomic three-client contract | Fixture/generator, web and native field builders/render sites, their contract tests | Current docs, input contract, output-facts handoff ruling, all affected version/fix records |
| M1: web facts and presentation | Live TV sidecar, page adapter and controls, shared stats, CSS, focused web tests | Uses M0 field contract; any necessary schema correction includes all consumers in the same PR |
| M2: Apple | Live TV DTOs, collector/adapter, shared panel, focused tests | Apple VOD adapter where labels are hardcoded; required build records |
| M3: Android | Live TV DTOs, collector/adapter, shared panel, focused tests | Android VOD adapter where labels are hardcoded; required build records |
| M4: qualification | Integration evidence and current docs | Contract reconciliation, screenshots, version records required by policy |

Read current [AGENTS.md](../../AGENTS.md),
[web shell map](WEB-SHELL-LAYOUT.md) and
[development pipeline](../DEVELOPMENT_PIPELINE.md). Establish the repository's
pinned compiler loop before any Rust edit. No Rust change is expected for
the inspected DTOs; discovering a missing required server field is a scope
decision, not permission to invent an API. Keep source-only archive and
exact-base verification rules if Rust work becomes necessary.

## 5. Normalize evidence before producing labels

### 5.1 Existing fields and concrete adapter gaps

Re-verify these symbols on the intended implementation base.

| Surface | Existing contract | Work required |
|---|---|---|
| [Server](../../crates/plurxd/src/live_tv_delivery.rs) | `LiveDeliveryPlan.source`, `.output`, `.video_action`, `.audio_action`, `.reasons`, `.deinterlace`, `.deinterlace_output` | Read-only reference; these are delivery facts, not a new API design. |
| Server source | Width/height, codec, `field_order`, `frame_rate`, `sample_aspect_ratio` | Normalize independently; optional fields may be absent. |
| Server output | Planned width/height, codecs, `frame_rate`, audio channels | Never label this object measured. It contains no output SAR or explicit measured scan field. |
| [Web](../../crates/plurxd/src/web/pages/live-tv.js), `liveTvStatsTelemetry` | Delivery available from status/current lease; browser dimensions; channel source details | Prefer matching delivery source; forward reasons; distinguish the three dimension types. |
| [Apple DTO](../../clients/apple/Sources/LiveTv.swift), `LiveTvDelivery` | Output/actions/packaging; currently no decoded source or reasons | Add optional decoding for existing source, reasons and deinterlace facts, with old-server fixtures. Preserve existing snake-case decoding strategy. |
| [Android DTO](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvApi.kt), `LiveTvDeliverySource` / `LiveTvDelivery` | Source currently retains only `field_order`; no reasons; defaulted `deinterlace` is used by display-mode matching | Add optional source facts/reasons and information-only presence tracking. Keep the playback boolean and its default behavior unchanged. |

Do not alter the meaning of an existing boolean used by playback selection.
If a playback DTO intentionally defaults a missing value, the information
adapter needs a separate way to retain missingness. Old JSON must keep
decoding; unknown future reason codes must remain displayable.

### 5.2 Internal evidence model

The following is a semantic contract for new client-local types, not a
literal server response or a required shared language implementation:

```typescript
type Availability = "known" | "pending" | "unavailable";
type Provenance = "source_probe" | "channel_observation" |
  "server_plan" | "stream_sample" | "player_presentation";
type FrameSize = { width: number | null; height: number | null };
type Ratio =
  { kind: "exact"; numerator: number; denominator: number } |
  { kind: "approximate"; value: number };
type Scope = { sessionId: string; attachmentRevision: string };
type Evidence<T> = {
  value: T | null;
  availability: Availability;
  provenance: Provenance;
  scope: Scope;
  missingReason: string | null;
};
type PictureFacts = {
  sourceFrame: Evidence<FrameSize>;
  sourcePixelAspect: Evidence<Ratio>;
  streamFrame: Evidence<FrameSize>;
  streamPixelAspect: Evidence<Ratio>;
  playerDisplay: Evidence<FrameSize>;
};
```

Map `attachmentRevision` onto existing ownership: web `LIVE_TV.serial` and
`LIVE_TV_LEASE.currentGeneration`, Apple controller `serial`, Android
`LiveTvPlayer.serial`. It is a conceptual type, not a new counter or wire
field. Do not add parallel ownership tokens or a server protocol revision. A channel fallback
carries the target UI scope plus its actual channel identity and observed
timestamp, and remains explicitly `channel_observation`. It cannot thereby
become a session probe. Optional observed timestamps belong only to producers
that actually supply them. `plan.source` remains valid for its active session;
it must not inherit the channel observation's 20-minute expiry. The channel
fallback retains its existing expiry and observed timestamp, is labeled
`Last observed broadcast`, and never drives a current-stream comparison.
Preserve provenance separately for codec, scan
type and cadence too; a measured size cannot upgrade planned codec metadata.

Dimensions are positive finite integers; invalid/zero values are missing,
not a frame of zero pixels. Accept a known height without inventing width.
Parse source SAR as a positive rational; empty strings, `N/A`, `0:1`, zero
denominators, negative/non-finite values and overflow are unavailable.
Use checked arithmetic/reduction for ratio products on native platforms
and safe-integer checks or exact integer arithmetic on the web. Do not let
malformed metadata crash the panel or change playback.

**Output selection:** eligible attached-stream sample facts outrank planned
output, component by component. Prefer a coherent sample format when one
exists. Keep the plan separately accessible in Diagnostics when the two
disagree. Never fill a missing component with a different rendition's fact.
Never promote browser presentation size, Apple presentation size, requested
quality or a source-shaped manifest into measured encoded dimensions.

**Actual producers in this repair:**

| Platform | `streamFrame` | `streamPixelAspect` | `playerDisplay` |
|---|---|---|---|
| Web | Matching `delivery.output`, `server_plan`, or unavailable | Unavailable; no output-SAR collector | Attached `videoWidth`/`videoHeight`, `player_presentation` |
| Apple | Matching `delivery.output`, `server_plan`, or unavailable | Unavailable; presentationSize supplies no output SAR | Attached item `presentationSize`, truncated to integers, `player_presentation` |
| Android | Attached `Player.videoSize` with positive dimensions and zero `unappliedRotationDegrees`, `stream_sample`; otherwise matching plan with visible provenance | Positive finite `pixelWidthHeightRatio` from that same eligible sample, approximate | Unavailable: `Not reported by this player` |

Android's sample describes the decoded, cropped frame, not automatically
the codec's padded storage allocation. Keep that basis in the evidence note.
A known crop/basis mismatch with source dimensions prevents a resize verdict
while retaining the measured frame fact. Reject nonzero unapplied rotation
for sample comparisons in this repair rather than swapping dimensions by
guess. Distinguish the resulting planned fallback from a sample measurement.

Preserve Android's SAR float as approximate; do not stringify it and parse it
as an exact source rational. A recognizable ratio within absolute 1e-4
(e.g. 40:33) may read `≈40:33`, never silently become exact. Exact 1.0 may
read `1:1`; otherwise retain the approximate value. Recognition tolerance
is for the label only and never expands the comparison tolerance in §6.2.
Measured-output acceptance has a real Android producer; synthetic web/Apple
measured fixtures prove the formatter only, not an available live collector.

### 5.3 Compare only facts that belong together

All session facts must match the current attached session and local
attachment revision. For a copied or encoded stream, the source must come
from that delivery's source probe to support a comparative verdict.

Reuse the existing fences and add regression coverage: web `Lease.status()`
returns a result only when its captured `generation` still matches and
`this.current === info`; Apple checks `serial == expected` after awaits;
Android checks `mine == serial`. Source and output come from one atomic
`delivery` object, so they cannot disagree in session identity when read
together. The additional association to verify is that player observations
belong to that same current delivery. Never mix source and output selected
from separate start/status snapshots.

At attach/start, capture the existing scope for each async read/callback.
Discard it if the owner changed before completion. Starting a replacement
clears comparative conclusions until new attachment facts are eligible;
do not briefly combine a new source with the old video element. Out-of-order
status responses within one attachment must not regress newer facts; reuse
the current request ordering or sample ordering mechanism.

`LiveTvSessionStatus` has no session identity field. Bind it to the
identity of the request that fetched it. Do not invent or trust an absent
`status.session_id`. Source-format invalidation clears its derived aspect
and comparison. Old metadata may remain only as explicitly historical
diagnostic context, never as the active hero value.

## 6. Resolve independent dimension, aspect and conversion conclusions

### 6.1 Frame-size conclusion

Run this only with two complete eligible frame sizes; otherwise leave the
comparison unavailable. Compare frame pixels without pixel-aspect
multiplication. Android sample sizes have a decoded/cropped basis; a known
crop or orientation mismatch suppresses the resize conclusion, not the
measured value.

| Comparison | Wording with measured output | Wording with planned output |
|---|---|---|
| Both axes equal | Frame dimensions unchanged | No resize planned |
| Neither axis larger, at least one smaller | Stream resolution reduced | Resolution reduction planned |
| Neither axis smaller, at least one larger | Stream frame dimensions increased | Larger frame dimensions planned |
| One larger and one smaller | Stream frame dimensions changed | Frame dimensions change planned |

Prefix the conclusion with the actual video action when encoded. For
`video_action=copy`, show
`Original video · repackaged for this player` only as the server's delivery
decision. If sample facts conflict with that plan, expose the conflict as
`Stream differs from the delivery plan`; do not override the observation or
claim a successful copy has been verified.

Frame-size equality never proves unchanged quality, aspect, scan type,
frame rate or audio. A measured decrease does not acquire a reason merely
because a maximum-resolution setting exists; use the delivered reasons.

### 6.2 Aspect conclusion

Derive source DAR from source frame and SAR only when both are valid. Derive
output DAR independently from a verified output sample's frame and SAR.
Do not reuse source SAR as output SAR, particularly after scaling.

There are two different conclusions:

1. **Display consistent with source shape.** Requires source DAR and an
   eligible presentation measurement with compatible aperture/orientation.
   Use a truncation-aware one-pixel tolerance on the compared axis: accept
   floor or ceil of `displayHeight × sourceDAR` for width, or floor/ceil of
   `displayWidth / sourceDAR` for height. Either comparison may establish
   agreement; do not require a documented browser choice of corrected axis.
   One pixel is the minimum rounding allowance and the chosen default, not
   a larger percentage tolerance. Unknown crop/aperture/orientation prevents
   the conclusion. A failed presentation check alone is not a stream warning.
2. **Stream aspect agrees/differs.** Android's eligible sample frame/SAR is
   the available producer here. Require a current source DAR and compatible
   crop/orientation basis. At output frame height compare the source-shaped
   width with the independently measured output-shaped width, using the
   same one-pixel floor/ceil allowance and retaining approximate-float
   provenance. Beyond that, report the difference without inferring cause.
   Missing output SAR means `Not verified`. Do not enlarge the tolerance
   just because an approximate SAR resembles a familiar fraction.

Keep exact rational facts in Details/Diagnostics. Use familiar 4:3 or 16:9
labels without an approximation marker only for an exact reduced ratio.
For Android float-derived values use §5.2's explicit approximate label.
Never erase that provenance after deriving DAR. Pixel-aspect metadata
describes the whole transmitted frame, not the shape of a programme inside
encoded black bars. Do not detect, crop or reason about those bars here.

Conservative release behavior is intentional: if an existing platform
collector cannot establish the required aperture/orientation basis, show
source aspect and player size separately without an automated verdict.

### 6.3 Scan, cadence and conversion reasons

Keep source field order and output scan state separate. A known plan with
server deinterlacing can say `Progressive planned`. A copied interlaced
source with known field order (`tt`, `bb`, `tb` or `bt`) says `Interlaced`
with a source/copy-plan note, even when the TV itself deinterlaces for display.
The Android TV copy case must never read `Progressive planned`. Without
relevant source/copy or deinterlace evidence,
do not infer scan type from H.264 or a pixel height. Preserve rational frame
rates internally and keep frame rate distinct from interlaced field rate.

Display supplied server explanations as escaped text, joined with ` · `
using the fixture's existing `reason` list format and notes placement.
Preserve raw code/explanation pairs in Diagnostics; do not paraphrase into
an undocumented code-to-phrase table. Ignore blank explanations after
trimming. Deduplicate identical pairs; retain distinct reasons. Unknown
codes keep their supplied explanations. If no usable explanation remains,
say `The server did not provide a conversion reason.`

On web the forwarding shape is:

```javascript
reason: plan?.reasons?.map(r => r.explanation)
  .filter(s => typeof s === "string" && s.trim()).join(" · ") || null
```

Keep escaping in the renderer; raw metadata never becomes HTML. The mockup
uses the actual `video_incompatible` and `unsupported_interlacing` sentences,
not a newly inferred cause. An old payload with a code but no explanation
retains that code in Diagnostics and uses the missing-reason fallback.

Never interpret `video_incompatible` alone as proof that the codec itself
failed. Keep video, audio, packaging, dimension and interlace causes distinct.
Reason rendering must not create a retry, probe, lease renewal or new tune.

## 7. Put the facts into the established presentation contract

| Internal field | One label across clients and modes | Modes / availability |
|---|---|---|
| Existing `decode_resolution` | Player display size | All four modes; Android shows unavailable with explanation. |
| Existing `source_resolution` | Source frame | Overview, Details, Diagnostics; group heading may distinguish broadcast from original file. |
| New `stream_frame` | Stream frame | All four modes on all clients; measured, planned or unavailable with visible provenance. |
| New `source_pixel_aspect` | Source pixel aspect | Details, Diagnostics |
| New `source_display_aspect` | Source display aspect | Overview explanation, Details, Diagnostics |
| New `stream_pixel_aspect` | Stream pixel aspect | Details, Diagnostics |
| New `frame_comparison` / `aspect_comparison` | Frame comparison / Aspect comparison | Overview explanation, Details, Diagnostics |
| Existing `stream_format` | Stream format | Codec · scan · cadence only; no dimensions, in every client and mode. |
| Existing `reason` | Reason | Existing list/notes placement; explanations joined with middots. |

M0 owns the atomic contract migration. One field has exactly one label;
there is no Compact alias or Android label override. The source hero uses
`Source frame`, with `Broadcast` as a group heading/context where needed.
These labels, mode membership, placement and availability are assertions
shared by web, Apple and Android, not independent design choices.

Give `stream_frame` all four modes. Compact contains both dimension fields
plus its existing player-state and device-buffer fields, on every client.
On Android the display field is explicitly unavailable while Stream frame
can be measured. Keep one fixture-defined order and render four wrapping or
stacked facts rather than hiding unknowns through a new schema extension.
All field labels remain identical to the fixture in every mode. Existing
`available_on` behavior is unchanged; no per-mode availability key is added.

**Single stream-dimension owner:** `stream_frame` is the sole active stream
frame dimension row. Remove width/height from `stream_format` on Live TV
and VOD across all clients in M0. Keep codec/scan/cadence provenance there;
missing cadence or scan is omitted. Planned-versus-measured conflicts may
be shown in explicitly labeled Diagnostics context, never as competing
active values. A server descriptor named `stream_format` may still carry
width/height internally: the adapter routes those into `stream_frame`.
No wire rename or server change is implied.

The same ruling is written into the existing output-facts handoff's §1 and
formatting table so its future implementation cannot restore a dimension
string inside the visible Stream format row. That effort is not required
to ship this repair. Reconcile new IDs against the fresh base before editing.

**Producer semantics:** web/Apple use presentation facts in the display row
and planned output in the stream row. Android uses eligible `videoSize` in
`stream_frame`, its same-sample SAR in `stream_pixel_aspect`, and no fabricated
player display size. Compact can therefore show `Stream frame 704×480` with
`Measured stream` on Android without changing labels between platforms.

**Layout:** follow §3's frame-first Overview. Keep source/stream
columns aligned when space permits and stack them on narrow screens.
Provenance appears immediately below each frame value. The explanatory
text belongs beside the display measurement and conversion summary, not
inside an optional hover tooltip. Retain track, buffer, reception and other
existing sections even where the prototype omitted them for focus.

**Missing data:** `Planned output`, `Measured stream`, `Last observed
broadcast` and `Unavailable` must remain visually distinguishable without
color. Missing reason or aspect metadata does not acquire a reassuring
green badge. Keep playback controls and TV focus behavior unchanged.

## 8. Execute packages with focused acceptance

### 8.1 M0 — one atomic three-client contract PR

Refresh from main and record the base and existing producers. Editing
`tests/playback/playback-info-fields.json` selects both native suites on the
same PR through `validation/ci_scope.py`. M0 must therefore include all
fixture consumers and pass their tests before M1, M2 or M3 exists. Do not
land a web-only field migration and plan to repair native clients later.

M0 owns these edits together:

- Canonical field IDs, labels, modes and provenance notes from §7; regenerate embeds and tables.
- Web `player/stats.js`, Live TV page render sites, both regex expectations
  in `player-dom.test.js` and `web-policy.test.js`, and field-selection tests.
- Apple `applePlaybackInfoFields`, `PlaybackInfoPanel`, hardcoded VOD labels
  in `PlayerView.swift`, Live TV baseline rows, and `AppleClientTests`.
- Android `InfoRow` builders, shared panel, hardcoded `PlayerScreen.kt` VOD
  labels, Live TV baseline rows, and `PlaybackInfoContractTest` (including
  label, placement, existing availability and four-field Compact assertions).
- The dimension-free `stream_format` ruling in this document and the
  existing output-facts handoff, both in the same commit.

This package is a compatibility bridge, not merely three label replacements.
Move existing dimensions into `stream_frame` with their existing evidence
basis; never parse a formatted string to invent provenance. Android's raw
`videoSize` moves into the stream row with the eligibility checks in §5.2,
and the display row becomes explicitly unavailable. New source/aspect facts
may remain unavailable until M1–M3 wire the DTOs and normalizers. No new
comparative verdict is required in M0. All clients must already render the
contract consistently and truthfully at the end of this PR.

**Per-package gate checklist, starting with M0:**

- Use `fix(<scope>): ...` for this observable repair, not `chore` or
  `refactor`. Current policy also accepts `perf(` for performance changes.
- Record actual focused regressions as `Regression-Test: <path>::<name>`
  lines in the PR description and landing commit. Use the repository's
  regression-field helper to generate landing lines; do not fabricate test
  names or evidence for tests not run.
- Include applicable `tests/client-fixes.toml` source/test mappings for client
  fixes, including M0; follow the current mapping/commit policy.
- For Apple source edits run `make apple-build-bump` and include required
  generated build/version records. Android source edits need an increased
  `versionCode` and corresponding README version record. Reconcile counters
  against the fresh integration base; do not pick values from this document.
- If updating `STATUS.md`, keep it at or below 400 lines. Add/update this
  document's docs-index row in the same commit as the document.
- Commit through the tracked hook, run affected builds and focused tests
  locally, and record exact commands/results before pushing.

Run the web contract/policy tests, Apple field-contract tests and Android
field/Compact contract tests, plus affected native builds in M0. Reuse the
commands in M1–M3 but run every affected surface now; later packages are
not evidence for this PR. Inspect generator diffs for unrelated drift.

**Acceptance:** all three suites pass against one fixture on this exact M0
commit; no per-client/per-mode label exceptions; no active dimensions in
Stream format; Android sample dimensions no longer populate display size.
Future-package-only facts are explicit unavailable values, not invented
measurements.

### 8.2 M1 — web adapter, shared renderer and regressions

Own [the require-able Live TV sidecar](../../crates/plurxd/src/web/live-tv.js),
[Live TV telemetry and Compact rendering](../../crates/plurxd/src/web/pages/live-tv.js),
[the status poll/write path](../../crates/plurxd/src/web/pages/live-tv-controls.js),
[stats and VOD Compact rendering](../../crates/plurxd/src/web/player/stats.js),
[CSS](../../crates/plurxd/src/web/app.css), and
[player DOM](../../tests/web/player-dom.test.js),
[web policy](../../tests/playback/web-policy.test.js) and
[Live TV](../../tests/web/live-tv.test.js) tests. Use M0's field contract;
any required fixture correction carries every affected native consumer/test
in the same PR rather than deferring them again.

Place pure normalization/comparison functions in the existing UMD Live TV
sidecar, where tests can require production code. The page adapter consumes
those functions. Exercise `Lease.status()`'s existing generation/current
fence and the controls file's `LIVE_TV.status` assignment with delayed polls.
Reuse current ownership; add no new token. Read source and output from a
single delivery snapshot, then associate it with the attached player.

Update all render sites: shared Overview, hand-rendered Live TV Compact in
`updateLiveTvStats`, and the VOD Compact strip in stats. The page's one-second
info refresh is not permission to add a network poll or tuner renewal.

Execute real production normalization/rendering in tests; string searches
alone cannot prove evidence precedence or attachment fencing. Update VOD's
shared label without inventing encoded width. Preserve the cached-VOD
unknown-picture regression and diagnostic field reachability.

```bash
node scripts/player-contract-table --embed       # Regenerate embeds.
node scripts/player-contract-table --write       # Regenerate contract tables.
node tests/web/player-dom.test.js                 # Shared info and field reachability.
node tests/playback/web-policy.test.js                 # Shared contract and policy assertions.
node tests/web/live-tv.test.js                    # Live behavior regression.
scripts/js-check                                 # Shipped JavaScript syntax.
scripts/player-input-fence                       # Input/contract consistency.
make validation-lint                            # Governed file ownership.
```

Inspect generator diffs; do not retain unrelated generated drift. If new
focused test files are introduced, name and run them explicitly and wire
them into the existing test catalog. Do not widen CI triggers for this task.

**Acceptance:** source/display width mismatch does not imply a resize;
a smaller planned web output says reduction planned; missing SAR remains unknown; server reasons survive
adapter/renderer; a delayed predecessor response cannot populate the panel.

### 8.3 M2 — Apple decoding, facts and shared presentation

Own [LiveTv.swift](../../clients/apple/Sources/LiveTv.swift),
[LiveTvView.swift](../../clients/apple/Sources/LiveTvView.swift),
[PlaybackInfoPanel.swift](../../clients/apple/Sources/PlaybackInfoPanel.swift)
and [LiveTvTests.swift](../../clients/apple/Tests/LiveTvTests.swift).
Retain M0's `AppleClientTests` contract coverage and test the existing
`serial == expected` fence; do not introduce an attachment counter.
Reconcile hardcoded labels in any shared VOD adapter discovered on the base.
Test optional source/reason decoding against old and modern server payloads,
formatting and stale-attachment rejection. Treat `presentationSize` as a
presentation measurement throughout.

Run `make apple-build` for iOS/tvOS compilation. Run the focused Live TV
XCTest cases with the repository's generated project, actual test target
and an available simulator destination; record the exact `xcodebuild`
invocation rather than inventing a device UUID in this plan. Compile success
does not substitute for those formatter/decoder regressions.

**Acceptance:** phone and TV panels express the same semantics, old JSON
still decodes, every informational TV control/section remains reachable and
no change to `PlayerSurface` sizing or playback ownership is needed.

### 8.4 M3 — Android decoding, sample semantics and shared presentation

Own [LiveTvApi.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvApi.kt),
[LiveTvScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt),
[PlaybackInfoPanel.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackInfoPanel.kt)
and the existing playback-info/Live TV JVM tests. Add parameterized
format/provenance cases in those suites. Verify the shipped Media3 collector
semantics, including pixel aspect, crop and rotation. Use §5.2's actual
`videoSize` producer for measured stream facts, not a presentation row.
Extend tests for the existing `mine == serial` fence; do not introduce a
new serial. Keep the playback `deinterlace` default unchanged.

```bash
cd clients/android
./gradlew --no-daemon :app:testDebugUnitTest \
  --tests 'tv.plurx.app.player.PlaybackInfo*' \
  --tests 'tv.plurx.app.livetv.LiveTvTest'
./gradlew --no-daemon :app:assembleDebug
```

Add any newly created focused suite to the command. Use the repository's
pinned SDK/JDK/Gradle runtime, and preserve unknown-field compatibility.

**Acceptance:** no double aspect correction, sample dimensions stay honest,
old delivery payloads remain usable, and phone/TV focus/scroll behavior is
preserved. Planned progressive scan does not arise from a missing boolean.

### 8.5 M4 — integration, evidence and promotion

Capture the production panels on desktop, narrow web, Apple phone/TV and
Android phone/TV, including compact and long-reason cases. Exercise keyboard
and TV remote navigation. Verify no new periodic requests or tuner sessions
are introduced when the panel opens. Record unavailable device evidence
explicitly rather than presenting the prototype as runtime qualification.

For a real SD session, inspect an already authorized source sample and its
matching output segment; record width/height, SAR/DAR, field order and cadence
alongside the session plan and panel capture. Reuse existing diagnostics;
do not open a second tuner stream or add an ongoing probe. If those samples
are unavailable, record the session as unverified; do not strengthen the UI
claims to compensate. A fixture proves formatting, not hardware behavior.

Freeze task merges, integrate current main, run exact-tree focused checks
and affected builds, and follow the current main-bound review/gate contract.
Use the current policy for qualification receipts and version counters.
Do not copy an older document's obsolete no-tests lane description; the
development pipeline includes later amendments. Merge and deploy are
separate operations. This design review does not replace implementation PR
review or authorize deployment.

## 9. Acceptance matrix — what must fail before the repair

| Fixture or event | Required result |
|---|---|
| 704×480, SAR 40:33, display 853×480, planned output 704×480 | Frame comparison says no resize planned; display is separate; never claim measured preservation. |
| 704×480, SAR 10:11, display 640×480 | 4:3 source; no automatic 16:9 inference. |
| Square-pixel 1280×720 copy | Report server copy decision; show provenance of output dimensions. |
| Android sample: 1920×1080 source, measured 1280×720 stream, rotation 0 | Real reduction from the actual `videoSize` producer, even if the player occupies a large window. |
| Web/Apple: 1920×1080 source, plan 1280×720 | Reduction planned; never measured from presentation size. |
| Android producer: planned 1280×720, measured 1920×1080 | Measured value wins; plan discrepancy remains visible. |
| Android producer: measured frame equal, sample SAR differs | Size can be unchanged while aspect differs; conclusions remain independent. |
| Source SAR absent/invalid | No inferred source DAR or aspect-preservation claim. |
| Output SAR absent | Stream aspect comparison is not verified; source SAR is not substituted. |
| Crop/aperture/rotation basis unknown | Facts remain visible; automated aspect conclusion withheld. |
| Output width missing, height known | Height-only label; no source/display-width substitution. |
| Android producer: copy metadata contradicts eligible measured frame | Surface the plan conflict; no unconditional verified-original claim. |
| Encoded output unavailable | Explicit absence; codec conversion is not treated as resolution change. |
| Generic incompatibility reason | Do not narrow it to a codec or deinterlacing failure. |
| New/unknown reason code, HTML-like explanation | Safely render explanation as text; no execution or silent loss. |
| Old-server payload lacks source/reasons/deinterlace | Decode successfully; no false scan/shape conclusions. |
| Channel switch, replacement, delayed status/observer | Never combine source, plan and player from different attachments. |
| Anamorphic VOD, cached VOD with unknown player size | Truthful shared labels; no fabricated Live TV or encoded facts. |
| Android TV copy: MPEG-2, field order tt, deinterlace false | Original video; Interlaced from source/copy plan, never Progressive planned. |
| Android sample includes non-square pixel ratio | `stream_frame` is measured; SAR is approximate; display size unavailable; never apply aspect twice. |
| Android sample has unapplied rotation or invalid dimensions | Reject sample comparison; label plan fallback planned, or show unavailable. |
| Source/display rounding: 853.33 reported as 853; alternate axis rounded | Floor/ceil comparisons pass without knowing which axis the browser corrected. |
| Live session exceeds channel source TTL | Matching `plan.source` stays visible; channel fallback alone expires after 20 minutes. |
| Shared field contract changes | Web and both native contract suites pass in M0 before any later package lands. |
| All Live TV/VOD Stream format rows | Codec/scan/cadence only; dimensions appear solely in Stream frame. |
| 320 px width, large text and TV navigation | No clipped facts or inaccessible sections; meaningful labels remain visible. |

## 10. Execution ledger — update this in each package

The current execution record is [the status page](PLAYBACK-INFO-DIMENSIONS-STATUS.md).
The table below records the imported handoff's package states as of
2026-09-25. `Implemented; unverified` is not a completion claim. The user
requested one integrated PR, so package PR cells are represented by commits
on that branch. Local focused checks passed after the adversarial review;
the current-head fast lane remains pending. If main moves, qualify the new
candidate.

| Item | State | Evidence required / current record |
|---|---|---|
| Chosen UI and renderings | Prepared | Illustrative captures in §3; not production acceptance |
| Current behavior reference | Refreshed | Clone based on `415eb047f3`, rebased onto `60f3803d1`, then merged `c99a29090`, `196d2a43e`, and `1d68af6eb`; see status page. |
| Fable design review | Corrections incorporated | B1–B4 and N1–N7 in §11; no second review claimed |
| Fresh base and toolchains | Recorded | Actual base, branch and runtime versions in status page. |
| M0 cross-client contract | Local checks passed | Integrated commit `148829768`; generated fixture and focused client checks passed. |
| M1 web | Local checks passed | Three focused Node suites and the player input fence passed. |
| M2 Apple | Local checks passed | iOS/tvOS simulator builds and 83 selected tests per target passed. |
| M3 Android | Local checks passed | Debug assembly and 35 selected tests passed. |
| M4 integrated candidate | Review addressed | One adversarial review completed; current-head fast lane and merge state are live on PR #526. |
| Live Cozi source/output comparison | Unavailable | No real Cozi session was available; planned output remains labeled as planned. |
| Promotion / deployment | Tracked on PR #526 | Merge only after the current-head gate; no deployment is part of this repair. |

For each task PR, record the user-visible change, behavior-based regression
names, commands run and material limitations. Keep `Regression-Test:` lines
in the PR and landing message. Screenshots must identify prototype versus
production and the source commit. Do not copy a pending row to completed just
because its PR was opened.

## 11. Fable review disposition — 2026-09-25

| Finding | Disposition in this revision |
|---|---|
| B1: fixture migration breaks native gates | M0 is one cross-client contract PR, with every field builder, hardcoded site and contract test. One label per field; four-field Compact keeps Android sample and display semantics separate. §§4, 7, 8.1. |
| B2: measured states lack a producer | Android `videoSize` supplies eligible stream dimensions/SAR; web and Apple remain planned-only for stream frames. Float provenance and rotation/crop limits are explicit. §§5.2, 6, 7, 9. |
| B3: duplicate dimension rows | `stream_frame` owns dimensions; `stream_format` owns codec/scan/cadence. The same ruling and examples are amended in the existing output-facts handoff. §§7, 8.1. |
| B4: web ownership incomplete | M1 owns both sidecar and controls, pure helpers and all Compact sites; web-policy tests are at `tests/playback/web-policy.test.js`; existing lease/serial fences are named and tested. §§5.3, 8.2. |
| N1: interlaced Android TV copy | Current behavior and copy-interlaced acceptance updated; no false progressive claim. §§2, 6.3, 9. |
| N2: rounding convention | Explicit floor/ceil, one-pixel allowance on either compared axis; no undocumented platform convention required. §6.2. |
| N3: invented reason paraphrase | Supplied explanations joined with middots; actual sentences in the SD mockup. §§3, 6.3. |
| N4: missing landing/version mechanics | Per-package regression trailers, fix mappings, counters, STATUS bound and index ownership added to M0 checklist. §8.1. |
| N5: channel observation expiry | Session source does not inherit channel TTL; historical fallback keeps its own expiry. §§5.2, 9. |
| N6: separate Compact renderers | Live TV and VOD hand-rendered strips explicitly owned and tested. §8.2. |
| N7: concrete attachment tokens | Existing web serial/generation and native serials named; no parallel counter introduced. §§5.2–5.3. |

**Review history:** the authoring task changed documents and renderings only.
M0–M4 remain unimplemented. Fable has not reviewed these dispositions; the
user has requested this build handoff with them incorporated. The four-field Compact layout and new field membership must pass all
three contract suites in M0; they cannot be deferred to M2/M3. No fixture
availability-schema extension is introduced.

Fable also reported missing mainline ancestry for Android PR #509. That is
outside this information-panel repair and has not been investigated or
re-landed here. This review does not authorize changing its code or history.


## 12. Concrete implementation recipes

### 12.1 M0 patch order and complete cross-client ownership

M0 is intentionally wider than a web task. Its PR must pass the shared
fixture's native fan-out before any later package exists. Make these edits
as one coherent change; do not publish intermediate inconsistent commits.

| Order | Files/symbols | Required change |
|---|---|---|
| 1 | `tests/playback/playback-info-fields.json` | Fix labels, add the new fields in §7, set Stream frame to all four modes, keep existing format/placement conventions. |
| 2 | `scripts/player-contract-table --embed` and `--write` | Regenerate web field embed and input-contract tables. Inspect the diff; discard unrelated generated drift without discarding user changes. |
| 3 | Web `player/stats.js`, `pages/live-tv.js` | Build the new rows, remove dimensions from Stream format, update shared Overview and both hand-rendered Compact paths. |
| 4 | Apple `Sources/PlayerView.swift` | Update `applePlaybackInfoFields`, value routing and all hardcoded VOD labels; add same field membership/order as fixture. |
| 5 | Apple `Sources/PlaybackInfoPanel.swift`, `LiveTvView.swift` | Four-field Compact, baseline Live TV rows, explicit unknowns and dimension-free Stream format. |
| 6 | Android `player/PlayerScreen.kt` | Update `PlaybackInfoDetails`, `InfoRow`, `playbackInfoRows` and hardcoded VOD labels; separate sample frame from display measurement. |
| 7 | Android `player/PlaybackInfoPanel.kt`, `livetv/LiveTvScreen.kt` | Four-field Compact, sample eligibility checks, planned fallbacks and explicit unavailable presentation dimensions. |
| 8 | `tests/web/player-dom.test.js`, `tests/playback/web-policy.test.js`, native contract tests | Change expected labels/membership; assert the new meanings, not just replacement strings. |
| 9 | Version/fix records, input contract, old output-facts handoff | Land the §7 ownership ruling, required mobile counters and regression mappings with the behavior change. |

Use this Compact field order everywhere: `decode_resolution`, `stream_frame`,
`player_state`, `client_loaded`. Match the fixture labels exactly. Render four
facts responsively; never drop the unavailable Android display row just to
preserve the old three-column layout. Native field-list arrays must retain
all eligible diagnostic fields exactly once.

At this stage only populate facts that already have trustworthy producers.
For web/Apple Live TV, use delivery output as planned. On Android, route the
eligible sample into Stream frame and retain the plan separately for later
comparison. VOD reuses existing eligible output metadata; a manifest that
cannot prove output dimensions becomes unavailable, not a copied source
size. No need to finish source-SAR collection or comparative banners in M0.

The actual source/stream comparison requires source facts from the same
session, not parsing the current `source_resolution` display string. Until
M1–M3 add those numeric facts, keep the comparison unavailable.

**M0 exit:** all field-contract tests and both native builds pass on the
same commit. No active dimensions remain inside Stream format. The fixture
can be consumed by all three clients without a later repair package.

### 12.2 M1 pure functions and web attachment wiring

Add production helpers to the existing `crates/plurxd/src/web/live-tv.js`
UMD sidecar and export them through its returned frozen object. The other
web scripts remain plain global scripts; do not add ES modules or a second
copy of the normalizer for tests.

Proposed function names and responsibilities (adapt names to existing
fresh-base equivalents; preserve the contract):

```javascript
parsePictureRatio(value)           // Exact positive rational or unavailable.
normalizeLiveTvPictureFacts({
  delivery,                       // One atomic current-delivery snapshot.
  presentation,                   // Positive browser width/height or absent.
  channelObservation,             // Explicit historical fallback, not source proof.
  attachmentCurrent,              // Boolean established by existing owner checks.
  nowSeconds
})                                // Typed facts with provenance and availability.
comparePictureFacts(facts)         // Independent frame/aspect conclusions.
formatPictureFacts(facts)          // Canonical values and provenance notes.
```

Do not make those functions read globals, the clock, DOM or network. Accept
required state as inputs; the page adapter owns current attachment checks.
For `attachmentCurrent=false`, return no current-player comparisons. Source
and output must be taken together from the selected delivery object; never
fall back separately across start/status objects.

In `liveTvStatsTelemetry`, first choose the current delivery using existing
owner state, then gather attached `videoWidth`/`videoHeight`, normalize,
compare and format. A browser presentation measurement goes only into the
display field. Web stream dimensions remain planned in this repair.
Forward supplied reasons as specified in §6.3.

In `pages/live-tv-controls.js`, test the path from the awaited
`LIVE_TV_LEASE.status()` result to `LIVE_TV.status` assignment. Capture/use
existing `LIVE_TV.serial` and lease ownership if an additional post-await
association check is necessary; do not add a new counter. A delayed response
from A after tuning B must neither overwrite B's facts nor clear a newer B
sample. Exercise both the sidecar fence and page-assignment path.

Use `plan.source` for active source facts after 20 minutes as well as at
startup. Apply expiry only to the channel-observation fallback. Missing
output dimensions never come from `videoWidth`; missing source SAR never
comes from channel/programme identity or a fitted CSS rectangle.

**M1 exit:** production formatter/DOM tests demonstrate planned SD, reduced
HD, copied video, unknowns, source expiry and a delayed predecessor poll.
Both Live TV and VOD Compact use the four fixture-defined rows. Opening or
refreshing the info panel adds no network calls.

### 12.3 Native DTO and normalizer implementation

Decode only existing wire fields. Add native source/reason structs as needed,
with optional fields and defaults that keep old payloads usable. Source fields
needed here are `width`, `height`, `video_codec`, `field_order`, `frame_rate`
and `sample_aspect_ratio`; reasons are `{code, explanation}` pairs. Keep
source SAR as the original string until validated. Native frame-rate structs
retain integer numerator/denominator.

**Apple:** retain `.convertFromSnakeCase`; use optional properties for the
new source/reason/deinterlace information. Keep `presentationSize` in the
display field, with its integer truncation accounted for. Output width/height
and cadence are planned delivery facts. Pure normalization/formatting helpers
must accept typed input without creating an AVPlayer or issuing requests.
Exercise them from `LiveTvTests` and retain the `AppleClientTests` fixture
parity tests. Existing `serial == expected` remains the owner guard.

**Android:** keep `ignoreUnknownKeys=true` and playback's existing
`deinterlace: Boolean = false` behavior. The value is used for display-mode
matching and must not change while repairing info. For information, true is
positive evidence; default false alone is never evidence of a reported false.
Decode optional `deinterlace_output` to preserve additional affirmative
information. Where raw key presence is already available, it may distinguish
explicit false from absence; do not restructure playback decoding solely to
show that diagnostic. Unknown/false without presence remains unreported for
information purposes. Copy plus source field order can describe an interlaced
stream without interpreting the defaulted boolean.

Read `videoSize.width`, `height`, `unappliedRotationDegrees` and
`pixelWidthHeightRatio` from the same attached-player sample. Only positive
dimensions with zero unapplied rotation become `stream_sample`. Retain SAR
as an approximate positive float except exactly representable 1:1. Never
multiply frame width by SAR before populating Stream frame. If the sample
is ineligible, use the matching planned output with Planned output provenance.
An absent display observation reads `Not reported by this player`.

Keep these helpers testable with ordinary data. The tests must invoke the
same production adapter that reads the sample fields, so measured outcomes
are not only fabricated values inserted after normalization. Test matching
source/output, sample-plan disagreement, non-square SAR, invalid dimensions,
rotation rejection and Android TV interlaced copy. Preserve `mine == serial`
and test stale callbacks. Do not touch display-mode policy, capability claims,
PlayerView resizing or deinterlacing behavior.

**M2/M3 exit:** old and modern delivery JSON decode; canonical fields and
mixed-version unknowns render correctly; existing native owner guards reject
predecessor facts. Both clients build, and Android's measured output is proven
through the actual sample adapter.

### 12.4 Copyable baseline fixture and mutations

Use this as the SD **web/Apple** delivery fixture. It is fabricated test data,
not an observed session or an API endpoint to contact. Feed it to the pure
normalizer/DTO decoder at the delivery-object boundary:

```json
{
  "source": {
    "video_codec": "mpeg2video",
    "width": 704,
    "height": 480,
    "field_order": "tt",
    "frame_rate": {"num": 30000, "den": 1001},
    "sample_aspect_ratio": "40:33"
  },
  "output": {
    "container": "mpegts",
    "video_codec": "h264",
    "audio_codec": "ac3",
    "width": 704,
    "height": 480,
    "frame_rate": {"num": 60000, "den": 1001},
    "audio_channels": 2
  },
  "video_action": "encode",
  "audio_action": "copy",
  "packaging": "mpegts",
  "deinterlace": true,
  "deinterlace_output": "field",
  "reasons": [
    {
      "code": "video_incompatible",
      "explanation": "The active player did not claim the complete source video route."
    },
    {
      "code": "unsupported_interlacing",
      "explanation": "The player did not claim a usable deinterlacing path for this source."
    }
  ]
}
```

Supply attached presentation 853×480 separately. Expected: Source frame
704×480; Stream frame 704×480 with Planned output; Player display size
853×480; exact source SAR 40:33/DAR 16:9; no resize planned; output SAR not
measured; the two explanations joined with ` · `. Stream format includes
H.264 and planned progressive cadence, with no width or height.

Derive distinct test cases by changing inputs rather than bypassing the
production normalizer:

| Case | Input change | Required assertion |
|---|---|---|
| 4:3 SD | SAR `10:11`, presentation 640×480 | Source DAR 4:3, no automatic 16:9 assumption. |
| Reduced HD | Source 1920×1080, SAR 1:1; plan 1280×720; only ceiling reason | Web/Apple say reduction planned and show only the supplied reason. |
| Android measured reduction | Same HD plan; sample 1280×720, SAR 1, rotation 0 | Measured reduction, not merely planned; display observation unavailable. |
| Android disagreement | Plan 1280×720; eligible sample 1920×1080 | Measured size wins; show separate plan discrepancy. |
| Android TV copy | Video action copy, output MPEG-2/704×480 and 30000/1001, deinterlace false, no output deinterlace mode; eligible sample | Original video decision; Interlaced from source/copy facts, not Progressive planned. |
| Ineligible sample | Width 0, nonfinite SAR, or rotation 90, varied independently | Invalid fields remain unknown; rotation rejects sample comparisons; fallback stays planned. |
| Unknown source aspect | Remove SAR or use `0:1`, `N/A`, negative/overflow rational | No derived source DAR or preservation claim. |
| Old payload | Remove source, reasons and deinterlace members | Native decoding succeeds and information stays unknown. |
| TTL | Advance clock past channel-observation expiry, preserve active plan | Active source remains; historical channel fallback expires if plan absent. |
| Predecessor poll | Start A, await status, attach B, resolve A | B's state remains intact; no mixed comparison. |
| Reason escaping | Unknown code with `<img onerror=...>` explanation | Literal escaped text; no DOM execution; no inferred cause. |
| VOD unknown | Source 3840×2160, no eligible output/player fact | No source-size substitution; Stream frame unavailable. |

Run the broader cases in §9 too, including partial dimensions and malformed
ratios. These examples do not replace testing rounding and format-basis
eligibility. Test names become regression evidence only after implemented
and actually run.

### 12.5 Exact local checks and native test destinations

Run commands in the isolated build checkout. Verify tools exist before
starting a package; configure the repository's pinned Android runtime.
Tests below are the existing focused suites, not a request to run every
repository runtime test after each edit.

```bash
# Web and shared contract (repository root):
node scripts/player-contract-table --embed
node scripts/player-contract-table --write
node tests/web/player-dom.test.js
node tests/playback/web-policy.test.js
node tests/web/live-tv.test.js
scripts/js-check
scripts/player-input-fence
make validation-lint
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check

# Both native compilation surfaces:
make apple-build
(cd clients/android && ./gradlew --no-daemon :app:assembleDebug)

# Focused Android contracts and live data behavior:
(cd clients/android && ./gradlew --no-daemon :app:testDebugUnitTest \
  --tests 'tv.plurx.app.player.PlaybackInfo*' \
  --tests 'tv.plurx.app.livetv.LiveTvTest')
```

For Apple, `project.yml` defines `plurx-iOSTests` and `plurx-tvOSTests`.
Pick existing simulator names from `xcrun simctl list devices available`;
the example defaults below match the repository's current naming and must
be replaced if unavailable. Use task-specific variables, not HOME or other
system variables. No signing or physical-device installation is needed for
these unit suites.

```bash
PLURX_INFO_IOS_DEST='platform=iOS Simulator,name=iPhone 17 Pro'
PLURX_INFO_TV_DEST='platform=tvOS Simulator,name=Apple TV 4K (3rd generation)'

(cd clients/apple && xcodegen generate)
(cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-iOS \
  -destination "$PLURX_INFO_IOS_DEST" -derivedDataPath build/DerivedData \
  CODE_SIGNING_ALLOWED=NO \
  -only-testing:plurx-iOSTests/AppleClientTests/testPlaybackInfoRowsMatchTheSharedFieldList \
  -only-testing:plurx-iOSTests/AppleClientTests/testMiniInfoIsAStripAndDoesNotSuppressTheIdleHide \
  -only-testing:plurx-iOSTests/AppleClientTests/testPlaybackInfoResolutionKeepsUnknownDistinctFromAValidPicture \
  -only-testing:plurx-iOSTests/AppleClientTests/testPlaybackInfoPreservesStoredModesAndAddsDetails \
  -only-testing:plurx-iOSTests/LiveTvTests test)

(cd clients/apple && xcodebuild -project plurx.xcodeproj -scheme plurx-tvOS \
  -destination "$PLURX_INFO_TV_DEST" -derivedDataPath build/DerivedData \
  CODE_SIGNING_ALLOWED=NO \
  -only-testing:plurx-tvOSTests/AppleClientTests/testPlaybackInfoRowsMatchTheSharedFieldList \
  -only-testing:plurx-tvOSTests/AppleClientTests/testMiniInfoIsAStripAndDoesNotSuppressTheIdleHide \
  -only-testing:plurx-tvOSTests/AppleClientTests/testTVPlaybackInfoFitsTheSafeCanvasAtCompactTenFootScale \
  -only-testing:plurx-tvOSTests/LiveTvTests test)
```

Verify the test runner executed the named tests: zero selected tests is not
a pass. Add any newly introduced test class/method filters to these commands.
If the fresh base renamed a symbol, locate its equivalent and record the
exact final invocation. When a fixture edit selects both native suites,
run both locally regardless of which client package prompted the change.
After focused checks pass, let the repository's required gate determine its
remaining selected suites; do not turn CI into the first compiler.

### 12.6 Document/source and integration discipline

Import this one handoff into the fresh base and index it in the same commit.
Keep the old output-facts handoff's visible-field ruling synchronized by a
targeted edit. It may retain an internal API object named `stream_format`;
only the display row loses dimensions. Do not confuse that ruling with a
wire rename or a request to build the separate output-facts effort.

After each task package, update §10 with commit/PR and actual evidence;
merge the validated task into the effort before starting the next. If
another task changed a shared fixture or field builder, reconcile on the
new effort head and rerun every affected consumer. A prior green native
build on a different base does not prove the integrated tree.

For the integrated main-bound candidate, freeze task merges, incorporate
current main, run exact-tree focused checks/builds, and follow current
repository review and promotion rules. Do not bypass gates, forge receipts,
add blanket suppressions or change CI triggers to make the package green.
An unavailable physical device is an evidence limitation, not permission
to claim runtime success. If a mandatory build environment is unavailable,
record the exact blocker and retain completed unaffected work.

## 13. Definition of done and Sol's final report

The code is complete only when all of these are true:

- The canonical fixture and web/Apple/Android consumers agree on labels,
  modes, placement and the four-field Compact order on the same commit.
- Broadcast/source frame, stream frame and player display are distinct;
  every visible value retains its evidence basis. Output dimensions appear
  once as the active stream frame, never inside Stream format.
- Web/Apple planned output stays planned. Android eligible samples produce
  real measured frame facts; defaulted booleans, rotation and approximate
  SAR never become unsupported certainty.
- Active source facts survive the channel observation TTL. Delayed statuses
  and player observations cannot cross existing attachment ownership.
- Actual server explanations render safely, including unknown codes and
  missing reasons. Old native payloads still decode.
- Every acceptance case in §9 is covered by a production-path regression
  or a recorded appropriate rendering/runtime observation. Tests named in
  PR evidence exist and passed; both affected native clients compile.
- Production panel captures, not only the proposal images, cover desktop,
  narrow web, native phone and TV layouts where available. Record missing
  physical/live evidence explicitly; no false playback claim fills the gap.
- Mobile versions, client-fix mappings, index/contract docs and current
  contributor requirements accompany the changes that owe them.
- The inline prototype, fake measurement toggles and temporary preview
  entry points are not shipped. Production views use actual normalized
  facts; fabricated example values live only in tests and documented previews.

Sol's final report must be concise and concrete: integration branch and
exact commit, task/integration PR links, what the viewer now sees, commands
and results, available production captures, unverified live/device cases,
and merge/deployment state. Link this completed ledger. Distinguish
implemented, tested, merged and deployed; none implies the next.

Do not close the implementation merely after building the web mockup,
renaming a label, or opening the first task PR. Finish all M0–M4 code and
required local evidence within the authorized task scope. The separate
PR #509 ancestry finding is not part of this build.
