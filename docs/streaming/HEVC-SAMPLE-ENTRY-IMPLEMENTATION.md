# HEVC sample-entry admission — Sol's implementation contract

**Status:** ready for implementation; no runtime code built by this document
· **Written:** 2026-09-16 UTC · **Executes:** Fable's F1–F10 review findings
and the two follow-up safeguards in the
[diagnosis, §1.2](AVATAR-SAFARI-DIAGNOSIS-AND-FIX.md).

Sol: read the [diagnosis and review disposition](AVATAR-SAFARI-DIAGNOSIS-AND-FIX.md)
first, then execute S01–S06 below in order. This file owns the implementation
contract; the diagnosis owns incident evidence. Do not reopen rejected
alternatives by quietly building a DV-only workaround. If current main has
changed the source-fact, capability, or delivery contracts materially, stop
and report the mismatch before substituting another design.

The source anchors were checked at **`3129ce993`**, the review base. The
documentation checkout, `10f2afe60`, is stale and has unrelated working
changes. **Do not build on that checkout's HEAD or sweep its edits into your
branch.** Fetch current main, create an isolated task worktree, inspect its
AGENTS instructions, and establish the pinned Rust compiler loop before
editing Rust. The [development pipeline](../DEVELOPMENT_PIPELINE.md) governs
PR and release mechanics; this handoff does not authorize merge or deployment.

## 1. Outcome — supported pictures get compatible packaging

An HEVC decoder claim is not permission to send every HEVC sample entry.
For otherwise compatible HEVC-in-MP4:

- Web and Apple clients that admit only `hvc1` must not receive an original
  `hev1` or `dvhe` MP4 as their playback plan.
- Select copy-video Remux, not video Transcode, when only packaging is wrong.
- Preserve DirectPlay for a source label the client actually admits.
- Preserve existing HDR/DV, resolution, audio, subtitle, and quality rules.
- Select a delivery whose **output** is compatible. A progressive remux can
  deliberately retain `hev1`/`dvhe`; “Remux” alone is not the guarantee.

Avatar is one complete-hvcC DV P8 reproducer. The review's census reports
111 SDR `hev1` MP4s, 28 compatible DV P8 `hev1`, 10 HDR10 `hev1` with minimal
hvcC, one DV P5 `hev1`, and 21 DV P5 `dvhe` with minimal hvcC: **171 exposed
rows**, not 171 device-tested failures. Build SDR and DV sibling regressions.
Apple's existing `dvTransport: hls` does not protect SDR/HDR10 admission.

### 1.1 Non-goals — boundaries that keep this repair reviewable

- No file-ID/title rules, permanent retagging, library rewrite, or cache purge.
- No HEVC disablement, blanket H.264 conversion, or HDR downgrade to repair a
  packaging mismatch. Existing unrelated encoding decisions still apply.
- No changes to HEVC parameter-set algorithms, DV strip/convert/preserve
  policy, or output tag selection. Reuse and test them.
- No `video_codec_tag` input to `CopyVideoOptions`, argv fingerprints, or
  fragment-index identity. It is a source-admission fact, not an output-byte
  recipe input; learning it must not invalidate existing indexes.
- No new `CAPS_Q` / `LegacyCaps` query parameter and no caps version bump.
- No general transport-capability redesign in `DeviceProfile`. Add only the
  execution guard and optional plan constraint specified in §5.
- No Android capability expansion in this PR; omission preserves its current
  behavior without making an untested new claim.
- No playback-control rewrite. [PR #336](http://192.168.4.7:3000/noirr/plurx/pulls/336)
  addresses the separate seek defect; record whether the acceptance candidate
  includes it rather than copying its implementation into this change.

## 2. Source fact — one parser, one stored value, both backends

The interfaces in code blocks below are **new required interfaces** unless
explicitly labelled existing. Resolve names against current source before
editing; equivalent local organization is fine, different semantics are not.

### 2.1 Add a nullable fact without redefining the video codec

Add to `ProbeResult` and `MediaFile` in
[domain.rs](../../crates/plurx-core/src/domain.rs):

```rust
pub video_codec_tag: Option<String>,
```

Use one extraction/normalization helper in
[scan/probe.rs](../../crates/plurx-core/src/scan/probe.rs). Read
`codec_tag_string` from the same **first non-attached video stream** used by
`parse_probe_json` for the video codec/profile. Do not use `streams[0]`, the
first arbitrary tag, a cover image, the audio tag, or the display HDR label.

Accept a four-character ASCII alphanumeric tag and lowercase it. Missing,
empty, wrong-type, wrong-length, non-ASCII, `[0][0][0][0]`, and `0000`
become `None`. Preserve other valid tags such as `avc1`; the gate applies
only to HEVC. Do not guess `hvc1` from `.mp4`, DV profile, or decoder support.

Use the repository's additive migration machinery for this SQL, shared
between SQLite and Hiqlite:

```sql
ALTER TABLE files ADD COLUMN video_codec_tag TEXT;
```

There is no default. Do not edit an already-applied historical migration.
Use `FILES_DOLBY_VISION_COLUMNS` and its backend installation paths in
[store/mod.rs](../../crates/plurx-core/src/store/mod.rs) as the precedent.
Cover file-column selection, positional row offsets, named row mappings,
fresh scan upserts, catalogue publication, import/export, and test builders.
Append positional columns where practical to reduce accidental offset shifts.

Audit these concrete surfaces, including equivalent symbols if main moved:

| Surface | Source |
|---|---|
| Traits, migration constants, catalogue access | [store/mod.rs](../../crates/plurx-core/src/store/mod.rs) |
| SQLite schema and row mapping | [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs) |
| SQLite writes | [sqlite/media.rs](../../crates/plurx-core/src/store/sqlite/media.rs) |
| Replicated file mapping and writes | [hiqlite_media.rs](../../crates/plurx-core/src/store/hiqlite_media.rs) |
| Catalogue schema/projection | [hiqlite_catalog.rs](../../crates/plurx-core/src/store/hiqlite_catalog.rs) |
| Publication | [hiqlite_publication.rs](../../crates/plurx-core/src/store/hiqlite_publication.rs) |
| Import | [hiqlite_import.rs](../../crates/plurx-core/src/store/hiqlite_import.rs) |

### 2.2 Backfill existing rows using the established bounded job

Follow `backfill_dolby_vision_facts` in
[state.rs](../../crates/plurxd/src/state.rs): one cluster-job lease,
**256 rows per tick**, ascending file ID, a per-node cursor, and a done stamp.
Use a separate job identity and settings; do not reuse the existing DV done
stamp, which may already be set and would skip every old HEVC row.

Required new logical keys, following existing key/local-job conventions:

```text
job lease: catalogue:video-codec-tag
done key: jobs.video_codec_tag_backfilled
cursor base key: jobs.video_codec_tag_backfill_cursor
```

Add the corresponding constants beside `JOB_DV_BACKFILL_DONE` and
`JOB_DV_BACKFILL_CURSOR`. Add a bounded `files_missing_video_codec_tag`
store operation on both backends. It selects rows after the cursor whose
tag is null and stored probe JSON is available; return enough source identity
and probe snapshot to fence an update. Reuse the parser from §2.1.

Required behavior:

1. Never open the media file or launch ffprobe for the backfill.
2. Walk past malformed JSON, missing video, and unknown/zero tags. Unknown
   stays null; it must not pin the cursor or cause an endless job.
3. Write only a recovered, valid tag. Guard against concurrent re-scan or
   replacement: the row's source identity and probe snapshot must still match
   what was parsed, and a newer non-null tag must not be overwritten.
4. Advance the cursor only after the bounded batch's updates complete. A
   failed write must remain retryable; do not stamp completion after failure.
5. Stamp done when no eligible rows remain ahead of the cursor. New scans
   populate the field themselves. Retrying a completed batch is harmless.
6. Verify both backends and the import/publication paths; the old DV migration
   is a pattern to reuse, not evidence that this new column is already safe.

Do not add JSON extraction expressions to each file query. They duplicate
the video-stream selection rule and bypass the single stored-fact contract.
Until backfill reaches a row, restrictive clients use the safe unknown-tag
behavior in §4; playback need not wait for a whole-library backfill.

## 3. Capability contract — an explicit progressive constraint

### 3.1 Wire shape and bounded validation

Add to Rust `DeviceCaps` and `DeviceProfile`, preserving defaults in named
profiles, `caps_profile`, and `DeviceProfile::from_caps_v2`:

```rust
pub progressive_hevc_sample_entries: Option<Vec<String>>,
```

The JSON spelling is exactly `progressive_hevc_sample_entries`. This is an
additive v2 field. Use Serde defaults; omit `None` when serializing where
appropriate. Its meaning is **labels admitted for original progressive
HEVC-in-ISO-BMFF**, subject to all existing codec/profile/display checks.
It does not constrain HLS's label set or grant a new DV profile.

| Value | Exact meaning |
|---|---|
| Missing or JSON null | `None`; legacy behavior, not a new compatibility claim |
| `[]` | Explicitly no admitted progressive HEVC sample entries |
| `["hvc1"]` | Admit hvc1 at this gate only |
| `["hvc1","hev1"]` | Admit both at this gate only |
| Valid DV entry | Still requires existing exact DV-profile/display support |
| Invalid non-null value | Reject; never turn it into `None` or permissive defaults |

Allow only exact lowercase `hvc1`, `hev1`, `dvh1`, `dvhe`; raw list length
0–4; no duplicates, coercion, trimming, unknown labels, or oversized strings.
Malformed JSON/types may use the existing extractor's 4xx rejection; semantic
violations must use typed **400 `invalid_capabilities`** with bounded detail
naming `progressive_hevc_sample_entries`. Do not echo unbounded input.

One shared validation helper must run at POST decision, create, and any
replacement/staging ingress that accepts a new caps document. It must run
before allocation, producer start, or the `plan_review_for` unusable-caps
fallback. A document with a non-null new field and an unsupported version or
otherwise unusable caps must be refused, not treated as a legacy trust case.
Documents without the new field retain the current compatibility paths.

### 3.2 Browser probes must distinguish progressive from MSE

In [web/index.html](../../crates/plurxd/src/web/index.html), keep existing
general decoder/HDR probes and their ceilings. Add a separate, bounded
progressive packaging probe; do **not** populate the new field from the
current `hevcTiersSync` or `hevcTiersMediaCapabilities` summaries. They OR
`canPlayType` with MSE, or `file` with `media-source`.

Required rules:

- Probe the existing HEVC tiers with each `hvc1.*` and `hev1.*` spelling.
  Use MediaCapabilities `type: "file"` where available; never accept a
  `media-source`/MSE success as progressive evidence.
- A usable `file` answer of `supported: false` wins; do not override it with
  a synchronous positive. If `file` queries are unavailable/throw for that
  tier, a nonempty media-element `canPlayType` answer is the fallback signal.
  If neither answers positively, do not claim that tier/entry.
- The list is not per-profile. Advertise a label only when its progressive
  evidence covers **every HEVC profile/ceiling being claimed** for direct
  playback, including the corresponding probed Main/Main10 rungs. One 720p
  Main success must not authorize a 4K Main10 source. Keep available PQ
  presentation checks distinct and do not let an MSE-only PQ result establish
  progressive presentation. When evidence cannot represent an asymmetry,
  omit the label conservatively; do not raise codec ceilings.
- Probe DV labels individually for the already-claimed DV profiles. The
  current `dvCan` ORs labels and transports; it cannot say which progressive
  label worked. A label must cover every relevant advertised DV profile
  before entering this flat list. Omission may cause a copy remux, never a
  fabricated DV claim.
- Emit the completed bounded result in `capsDocument` and its create payload.
  An empty result is `[]`, not omission. Recompute with the existing caps
  refresh lifecycle; do not persist a result across incompatible browser,
  decoder, or display changes without the existing invalidation rules.
- Use one settled caps snapshot for a decision and the create that acts on
  it. Integrate with `PLAY_CAPS_READY`; early playback must not briefly send
  an unrestricted claim while async packaging probes are unfinished.

Keep the probe count finite: the existing tier set × two ordinary HEVC tags,
plus the bounded existing DV-profile set. No polling loop or full media
download is required for capability discovery. Unit tests inject probe
answers; Safari/Chrome acceptance records the real answers separately.
Do not hard-code the review's predicted answers by user agent.

### 3.3 Apple participates in the same PR; Android retains omission

Update `DeviceCaps` and `Caps.capsDocument` in
[Caps.swift](../../clients/apple/Sources/Caps.swift):

```swift
var progressiveHevcSampleEntries: [String]? = nil
```

When claiming HEVC, emit `["hvc1"]`; otherwise omit. Verify the actual API
encoder produces `progressive_hevc_sample_entries`, not camelCase or a
second nested shape. Keep `dvTransport: "hls"`, supported DV profiles,
audio, display, and codec ceilings unchanged. This conservative constraint
does not claim a physical-device test that has not occurred.

The same snapshot must survive decision, initial create, resume, and
replacement payloads. Inspect [PlurxAPI.swift](../../clients/apple/Sources/PlurxAPI.swift)
and [PlayerController.swift](../../clients/apple/Sources/PlayerController.swift),
not just the struct initializer. Add serialization and request-body tests in
[AppleClientTests.swift](../../clients/apple/Tests/AppleClientTests.swift).
Compile iOS and tvOS; claim a new Apple build number using the repo workflow.
Do not hand-edit only one generated build/version surface.

Leave Android's producer absent for now. Verify an Android-shaped document
without the field preserves the old decision. Do not substitute an untested
blanket `["hvc1","hev1"]` claim.

### 3.4 A rejected new claim must not downgrade to a legacy request

`askDecision` currently retries GET on POST 400, 404, or 405. Capture the
exact caps snapshot sent on the POST and use it to decide fallback:

| Situation | Required client action |
|---|---|
| New field is non-null, including `[]`; POST returns 400 / 404 / 405 | Surface the failure; **no** legacy GET with the field removed |
| Typed `invalid_capabilities` or `unsupported_hevc_delivery` | Surface failure; do not retry by dropping caps, clearing a learned limit, or changing quality |
| Field absent/null; legacy-compatible error without a new refusal code | Retain the existing fallback behavior |
| 401/403, abort, network/5xx, unrelated validation failure | Retain the existing applicable error path; no new permissive fallback |

Do not detect the new field by list length: `[]` is the strongest constraint,
not “nothing to preserve.” Use stable error codes, not message-text matching.
Check Apple for any equivalent retry behavior and protect it too if present.

This is not a feature-negotiation protocol for old servers. A pre-fix v2
server can silently ignore an unknown field and return HTTP 200. Therefore
deployment must upgrade **all serving nodes before refreshing/publishing the
new clients**; mixed old-server enforcement is explicitly not guaranteed.
Record that limitation. Do not claim the POST-error guard solves silent
unknown-field acceptance. A negotiated server-feature handshake would be a
separate change, not something to improvise in this repair.

## 4. Pure decision — the source label participates in admission

In [playback/mod.rs](../../crates/plurx-core/src/playback/mod.rs), add one
shared packaging check, conceptually:

```rust
fn progressive_hevc_requires_normalization(
    file: &MediaFile,
    profile: &DeviceProfile,
) -> bool;
```

It returns true only when:

1. The normalized video family is HEVC/H.265.
2. The source container is `mp4`, `m4v`, or `mov`.
3. The profile has a non-null explicit new constraint.
4. The source tag is unknown or not in that constraint.

Use this alongside container admission in `evaluate`. Do not mark the
video codec itself unsupported. Auto chooses Remux if nothing else needs
video encoding; Original also remuxes while retaining no-video-encode
semantics. Forced Transcode and higher-priority conversion rules retain
their current behavior. H.264, audio-only, and MKV routing are unchanged by
this new gate.

Make the final reasons truthful and test them. Required stable wording for
a packaging-only Remux:

```text
HEVC sample entry hev1 not admitted for progressive playback; normalizing through copy-video delivery
HEVC sample entry unknown; normalizing for the reported packaging constraint
```

Substitute the validated known source tag in the first reason. If the final
method is Transcode for another reason, do not append “copy-video delivery”
as though it were the executed result. Keep the existing video/HDR reason.
`decide_forced(Original)` discards `evaluate`'s reasons and builds a new list:
explicitly preserve/add the packaging reason there through the shared helper.

Verify the derived recipe, not only this pure verdict. Relevant daemon
anchors are `review_client_plan`, `review_client_plan_inner`,
`plan_review_for`, create, and replacement paths in
[http/hls.rs](../../crates/plurxd/src/http/hls.rs). The same source fact and
caps must reach each. An absent-field legacy path remains supported; an
invalid present-field path must not enter it.

## 5. Delivery guard — do not turn one incompatible URL into another

### 5.1 Two builders remain different for a reason

| Path | Existing owner | Constraint |
|---|---|---|
| Segmented copy | `CopyVideoOptions::from_probe`, `copy_video_args`, `hevc_copy_tag_for_source` in [transcode/mod.rs](../../crates/plurx-core/src/transcode/mod.rs); [fragindex.rs](../../crates/plurxd/src/fragindex.rs) | Rebuild/promote initialization as needed; keep profile-aware hvc1/dvh1 and DV handling |
| Progressive copy | `progressive_hevc_copy_args`, `RemuxSpec`, `stream_mp4` in [http/stream.rs](../../crates/plurxd/src/http/stream.rs) | Promoted parameter sets require the existing hev1/dvhe behavior; do not blanket-retag |

Use `hevc_parameter_set_promotion_required` / existing copy options, not a
new duplicate “extradata length means tag” policy. The reviewed implementation
detects a minimal hvcC at 23 bytes. Avatar's 132-byte hvcC is a different
case. Calculate/inspect the **actual progressive builder's output tag**;
do not assume it has the same profile-aware choice as the segmented builder.
Include preserved P5 and compatible P8 cases in this comparison.

### 5.2 Keep transport refusal at the HTTP/execution boundary

After the final method/track decisions, use the original v2 caps and source
copy facts at the `delivery_plan` caller. Do not add a general transport set
to `DeviceProfile` or change every decision signature for this fix.

For an HEVC Remux with a present progressive constraint, determine whether
the progressive builder's output tag is admitted. If not, it **requires HLS**:

- If `caps.transports` explicitly includes `hls`, return Remux with an
  additive `requires_hls: true` execution-plan field. The field is optional,
  defaults false, and is omitted for unaffected plans. Add it to the Remux
  variant, not as a new playback method. Existing URLs may remain for wire
  compatibility, but updated clients must not execute the progressive URL
  when the flag is true.
- Otherwise, return typed **409 `unsupported_hevc_delivery`**, explaining
  that the available progressive copy violates the reported packaging
  constraint and HLS was not claimed. No `/direct` rescue, no implicit video
  transcode, and no response that suggests the incompatible remux URL works.

Explicit `[]` admits no progressive output, so a copy Remux requires HLS.
Missing transports with a new restrictive claim are not HLS evidence.
When the actual progressive output is admitted, keep existing routing;
this guard is not a general certification of fragmented progressive support.
Native Safari/Apple routing still takes HLS as it already does.

At HLS create, validate a present new constraint before allocation and
require an explicit HLS transport claim. Reuse the same typed refusal for
the incompatible request. Existing field-absent clients keep their behavior.
Test replacement/staging ingresses too; do not repair `/decision` alone.

The proposed `requires_hls` bit is the narrow execution constraint that
closes F3/F9, not a new transport-planning subsystem. It must survive DTO
serialization and native decoding. If current main already has an equivalent
field with these semantics, reuse it and record the mapping.

### 5.3 Execute and retain the constraint on clients

Update the actual routing inputs in
[playback-policy.js](../../crates/plurxd/src/web/playback-policy.js) and its
caller. `requires_hls` takes precedence over the progressive-remux branch,
including explicit audio selection and missing bitrate. With native HLS,
select native copy HLS; with a proven usable MSE HLS route, use the existing
copy-HLS adapter. If neither can execute HLS, report the delivery failure.

Carry the constraint into cold-index/recovery policy:
`indexPendingFallback` and other fallback branches must not return a
progressive-remux URL when `requires_hls` is true. Preserve the existing
bounded wait/error behavior; do not add an infinite index/retry loop.
Do not forge the flag on the client from a title, tag heuristic, or user agent.

Apple already uses HLS for remux, but add decoding/defaulting for the new
plan field and tests that no fallback violates it. Old responses without
the field must still decode. Android omission does not acquire new routing
behavior. Do not repurpose existing explicit quality controls to bypass
the packaging constraint. Distinguish Original's no-video-encode promise
from an explicit one-stream/progressive transport veto: if that veto and
`requires_hls` conflict, report the incompatibility rather than silently
returning unsafe progressive bytes or overriding the user's transport choice.

The raw `/direct` resource is not a new authorization boundary. This task
governs server-issued playback plans and updated client execution; it does
not prohibit an authorized caller from manually fetching original bytes.

## 6. Work packages — one coherent PR, focused evidence per step

Use one ordinary task branch unless current repository policy requires a
different integration workflow. All server, web, and Apple pieces belong
in the same final PR; do not mark a server-only or web-only slice ready.
The names below are **required new test labels**, not existing passing tests.
Use equivalent repository-style names if needed and record the mapping.

### S01 — source fact and bounded recovery (F4, F5)

Implement §2. Test `video_codec_tag_uses_first_non_attached_video`, store
round trips for both backends, publication/import, and the new backfill.
Include cover art first, a second video with a different tag, malformed JSON,
minimal hvcC, null/zero tags, source replacement during backfill, and more
than 256 eligible rows. A valid current scan overwrites stale facts with its
own result; an old snapshot must not overwrite that scan.

**Acceptance:** an already-scanned SDR and Avatar-shaped row acquire the
right tag without media access; unknown rows do not stall completion; changing
only the stored tag leaves the fragment-index identity unchanged. Compile
all targets to catch every `MediaFile` / `ProbeResult` literal.

### S02 — validated caps and shared admission (F1, F7, F8)

Implement §3.1 and §4. Pin `sdr_hev1_requires_copy_remux`,
`avatar_p8_hev1_requires_copy_remux`, and
`original_preserves_packaging_reason`. Construct otherwise compatible audio,
HDR, resolution, and quality facts so the test exercises this gate rather
than an unrelated pre-existing remux reason.

**Acceptance:** old code admits the SDR/P8 fixture; repaired code remuxes
with a reason, still preserving the appropriate grade. Add absent, null,
empty, invalid, and permissive-list tests; invalid present caps cannot reach
the create trust fallback. V2 defaults and named profiles still compile and
retain their old field-absent behavior.

### S03 — progressive web probes and no downgrade (F6, F8; follow-ups)

Implement §3.2 and §3.4 against the shipped functions. Do not test only a
parallel reimplementation. Use the repository's extraction/module harnesses.
Pin `mse_support_does_not_claim_progressive_hev1`,
`packaging_claim_covers_every_advertised_hevc_tier`,
`new_caps_failure_does_not_retry_legacy_decision`, and
`empty_packaging_claim_is_not_omitted`.

**Acceptance:** an injected Chrome-like progressive matrix claims both tags;
a Safari-like matrix claims only the tags its progressive probes accept;
MSE-only, false, partial-tier, thrown, and missing-API answers cannot widen
the claim. POST 400/404/405 with non-null new caps produces zero GET retries;
legacy-absent behavior remains pinned. Decision/create use one settled caps
snapshot. Record real browser answers later; these are deterministic units.

### S04 — Apple serialization and request propagation (F2)

Implement §3.3 using the actual JSON encoder and request types. Add tests for
HEVC-present `["hvc1"]`, HEVC-absent omission, SDR and HDR displays, unchanged
DV transport/profiles, create/resume/replacement propagation, and old-response
decoding. Update build metadata through the existing build-claim mechanism.

**Acceptance:** iOS and tvOS compile; the focused Apple capability/request
tests pass; serialized v2 requests contain the snake_case field. Do not claim
device playback from a simulator/unit result. An Android-shaped absent-field
document still has its baseline direct-play behavior.

### S05 — compatible execution and refusal (F3, F9)

Implement §5. Test the actual decision handler and session-create seam,
serialized `requires_hls`, routing, and cold-index fallback. Pin
`minimal_hvcc_hvc1_only_requires_hls`,
`progressive_only_incompatible_hevc_returns_409`, and
`required_hls_never_falls_back_to_progressive`.

**Acceptance:** the real progressive builder still emits the correct
hev1/dvhe for promoted input, but no updated constrained client selects it
when incompatible. Complete-hvcC, minimal-hvcC, P5, compatible P8, missing
bitrate, selected audio, and unknown tags are covered. No process/session
is allocated for a refused create. Existing DV and fragment-index tests pass
without changing their output-policy expectations to make this fix green.

### S06 — register evidence, qualify clients, and hand off the PR (F10)

Add the new route to
[routing-decisions.toml](../../tests/playback/routing-decisions.toml), update
[PLAYBACK.md](../PLAYBACK.md) and [API.md](../API.md), and register the focused
tests in normal web/Rust/Apple checks and the corrective evidence catalogue.
New standalone tests must actually run under the affected check, not merely
exist on disk. Add a dated result receipt under the appropriate evidence
location; any new prose document also needs a row in [docs/README.md](../README.md).

**Acceptance:** local compile/lint and focused regressions describe the exact
candidate head; browser/device evidence is recorded or explicitly pending;
the PR body lists commands, counts, exceptions, and F1–F10 coverage. Open
draft and follow the current independent review/gating process. Do not mark
runtime code independently reviewed because this design received a review.

## 7. Regression matrix — minimum cases, not a full product rewrite

| Layer | Required positive / negative cases |
|---|---|
| Probe/store | First playable video; cover/second track; canonical case; unknown/zero/malformed; both stores; import/publication; backfill fencing and restart |
| Decision | SDR and P8 hev1; HDR10/minimal hvcC; hvc1 admitted; hev1 explicitly admitted; unknown under restriction; H.264/audio/MKV unaffected |
| Quality/reasons | Auto; Original with exact reason; forced Transcode; existing HDR/subtitle/height rules still win truthfully |
| Wire validation | Absent, null, empty, valid subset, duplicate, unknown tag, uppercase, >4 entries, wrong type, wrong version plus present constraint |
| Web probes | Progressive yes/MSE no; progressive no/MSE yes; partial Main/Main10 tier coverage; explicit false vs fallback positive; errors/unavailable APIs; per-DV-label coverage |
| Downgrade | Present and empty constraints block POST-to-GET retry; typed refusal stays fatal; absent legacy path unchanged |
| Execution | Both copy builders; 23-byte and complete hvcC; progressive-only refusal; requires_hls serialization/default; missing bitrate; audio-selected route; cold-index fallback |
| Lifecycle | Decision/create/replacement receive the same fact and caps; invalid new caps cannot enter legacy trusted review; refused create has no producer |
| Native | Real Apple encoder spelling; SDR/HDR/DV; HEVC absent; iOS/tvOS compile; create/resume/replacement; Android omission |
| Output integrity | Segmented hvc1/dvh1 choices, hvcC parameter sets, DV metadata and playlist agreement; index identity unchanged by source-tag backfill |

Do not commit commercial clips. Use generated rights-safe media for the
ordinary SDR/hvcC integration cases and existing DV fixtures/argv contracts.
The private Avatar copy is manual diagnostic evidence, not a distributable
unit-test asset. Hash equality of its original experiment does not imply
that every legitimate production bitstream filter must leave every NAL byte
unchanged; no video encoder is the relevant packaging-only guarantee.

## 8. Validation — prove the actual branch before pushing

These are existing command families at the reviewed base; re-check target
availability on current main. Run the exact new focused tests as well, with
nonzero executed-test counts. Rust 1.97.1 is the reviewed pin; if the intended
base changes the pin, follow that base and record it rather than silently
using the shell's default Cargo.

```bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo check --workspace --all-targets --locked
rustup run 1.97.1 cargo fmt --all --check
rustup run 1.97.1 cargo clippy --workspace --all-targets -- -D warnings
rustup run 1.97.1 cargo test -p plurx-core playback::
rustup run 1.97.1 cargo test -p plurx-core scan::probe::
rustup run 1.97.1 cargo test -p plurx-core transcode::
python3 scripts/js-check
node tests/playback/web-control.test.js
python3 -m unittest discover -s tests/validation -p test_playback_routing_inventory.py
python3 -m unittest discover -s tests/operations -p test_docs_index.py
make apple-build
```

Use the backend-specific store test invocations selected by the changed
paths; the default feature set is not evidence for a backend it excludes.
Select the new Swift cases through the existing `apple-test` / Xcode test
workflow, using actual installed simulator destinations. Record their real
commands rather than inventing a scheme or claiming compilation as tests.
Use `apple-build-bump` and the Apple build receipt requirements as applicable.

Run the regression catalogue and history checks before the PR. Re-verify
compiler and focused results if the intended base moves. If the machine lacks
the pin, use the source-only loop in
[AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md); never send credentials or
use a push to CI as the compiler. Keep unrelated known failures distinct from
new failures and list explicit exclusions. Do not weaken a test to hide an
environmental FFmpeg/filter limitation.

## 9. Physical acceptance — frames, route, and grade must agree

Record candidate/server commit, client build, macOS/Safari/Chrome versions,
Apple device/OS, exact source facts, caps payload, decision and reasons,
`requires_hls`, selected route, copy argv, init/master codec/DV signaling,
index warmness, and observed result. Redact tokens and private URLs.

Required finite sweep:

1. **Safari SDR hev1 and Avatar-shaped DV P8:** establish the original-path
   control, then the repaired normalized path on the same machine. Require
   `video.getVideoPlaybackQuality().totalVideoFrames > 0`, increasing frames
   and currentTime, visible moving picture, and audio. If that API is absent,
   use an actual presented-frame callback or equivalent recorded evidence;
   a metadata/playing/TTFF event alone is not a substitute.
2. **Minimal-hvcC source:** prove the route is native copy HLS, not the
   progressive builder's incompatible output. Exercise a cold/missing index
   and show it does not switch to progressive delivery.
3. **Apple:** test at least one SDR and one HDR10 source on physical iOS and
   tvOS when available, plus compatible DV on an appropriate display. The
   client change and both builds are mandatory even if hardware acceptance
   must be reported pending.
4. **Chrome:** capture actual progressive hvc1/hev1 probe answers. An
   otherwise compatible hev1 source remains direct when its label is proved;
   do not assume every Chrome/macOS version has that answer.
5. **Controls:** generated hvc1 MP4 and H.264 MP4. The review found no hvc1
   MP4 in the library, so do not claim a nonexistent library control.
6. **Continuity:** target first visible frame within 15 seconds on the test
   network, then two minutes without terminal restart. Perform ten alternating
   seeks and one short seek burst; the final requested position wins. Measure
   each seek from request to first resumed frame, with a 10-second target,
   rather than excluding buffering time from the measurement. Report cold
   indexing separately. Record whether the seek fix is in the candidate.

The bounds are acceptance targets, not historical measurements. Check HDR/DV
presentation separately from decode success. A 48-frame AVAssetReader pass
is useful evidence but does not qualify Safari or a physical Apple display.
If access is blocked, finish software evidence and report the missing device
checks honestly; do not deploy a candidate on a claimed device pass that never
happened.

## 10. Delivery to Paul — what “done” means

Provide the draft PR link, exact head/base, implemented work-package list,
F1–F10 disposition, and the two added safeguards. Include local commands and
test counts, both Apple build results, measured browser answers, device
acceptance status, and remaining exceptions. Keep schema rollback additive;
do not drop the new column or undo source media as part of rollback.

State the rollout constraint explicitly: enforcing servers first, then web
refresh/Apple client publication. New servers with old clients retain the
old absent-field limitation; old servers may ignore the new v2 field.
Do not declare every library row repaired from a census or unit fixture.

No automatic merge, server restart, image deployment, App Store/TestFlight
publication, or library mutation is authorized by this handoff. If a required
acceptance or release step needs new access or authority, identify it and
request it instead of expanding scope.
