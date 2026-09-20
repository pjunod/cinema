# Android Dolby Vision — bounded delivery repair

**Status:** building on `codex/android-dv-delivery` · **Base:** `0f1e5e43` ·
**Updated:** 2026-09-20 UTC · **Executes:** the Fable-reviewed
[root cause and proposed fix](ANDROID-DV-CONVERSION-RCA-AND-FIX.md).

Progress: server and Android implementation are complete; pinned Rust 1.97.1
and Android debug Kotlin compilation are green. The single merge-boundary
adversarial review found five issues, all addressed: converted-index identity,
HTTP boundary coverage, Android lifecycle coverage, complete native-DV mux
evidence, and correlatable v2/emitted-package diagnostics. The server narrows
conversion by request transport, marks converted remuxes as HLS-required,
re-decides concrete progressive requests, and uses structured source facts
plus preserved-DV muxer strictness. Android build 109 retains the selected
transport through initial play, seek, reopen, track changes, recovery,
failover and prepared handoff. The one final focused test pass has not run yet.

Build one correction to the server/Android delivery contract: send Profile 7
conversion through the existing HLS converter, retain that transport through
Android playback changes, and preserve valid Dolby Vision configuration on
native-DV progressive copies. Older clients receive a truthful compatible
base when they request a progressive path that cannot convert.

This is **one main-bound PR**, with two implementation commits and focused
validation. The steps below are work inside that PR, not separate task PRs
or a new effort programme. No runtime qualification gate, device allow-list,
rollout cohort, proof receipt, new converter, or library rewrite is needed.

## 1. Outcome and limits — finish the actual repair

The motivating device is Lenovo `TB322FC`, whose display reports DV and whose
non-secure hardware decoder reports Profiles 5 and 8. File `5418` is Profile
7 with an HDR10-compatible base; the incident decision selected conversion
but Android build 107 opened the progressive executor. That executor neither
converts RPUs nor consistently writes the DV configuration box. The RCA
separates observed device facts from predicted incident bytes; preserve that
distinction when recording implementation results.

The completed change must deliver these outcomes:

1. P7 → P8.1 remux decisions require HLS even without sample-entry claims.
2. Android executes that requirement on initial play and subsequent changes.
3. A stale APK or saved progressive URL gets valid compatible-base playback
   when that fallback is supported, without a new transport-only 409.
4. Native P5/P8 preserved progressive copies contain the correct DV record
   and sample entry. P7 narrowing and this packaging fix ship together.
5. Diagnostics distinguish requested policy, emitted media, and decoder
   selection. A DV badge alone is never recorded as playback proof.

Leave Apple/web execution, converter algorithms, index scheduling, cluster
takeover, subtitle architecture, and generic playback recovery alone. Keep
existing valid narrowing in takeover/legacy HLS paths. Fix a newly discovered
defect there only if it prevents this bounded contract; otherwise record a
separate issue. Do not turn this repair into a fleet certification project.

## 2. Product policy — no software enablement gates

Use the existing `DV_CONVERT` setting and `dv_convert_enabled()` behavior:
absent means enabled; an explicit operator choice remains authoritative.
There is no new feature flag and no requirement to opt in after installation.

Do not make execution depend on a passing test receipt, an observed decoder,
a Developer-page status, a model name, a performance score, a rollout
percentage, or prior successful playback. Missing diagnostic evidence means
unknown evidence; it must not switch the feature off.

The implementation still selects a route that can perform the requested
operation. For example, progressive cannot convert P7, while copy HLS can;
a P5-only decoder cannot consume P8. These are source, transport and decoder
facts used to choose playable media, not a new feature-enable mechanism.
Never falsify those facts to force DV output.

**No settings UI work is required.** If a small explanation is necessary,
extend the existing Developer entry only: the existing enable/disable
control, requirements, and met/unmet/unknown status. Requirement status is
advisory and cannot disable the control or feed a runtime admission predicate.
Do not introduce another toggle or alter an unrelated permanent file
conversion workflow. No regular Settings page or player popup is needed.

## 3. Ownership and sequence — one PR, two coherent changes

Start from current `main` on `codex/android-dv-delivery` (or an equivalent
`codex/` branch). Keep unrelated working-tree documents out of the patch.
Establish the pinned Rust compiler loop **before** editing Rust (§8).

| Step | Owned files / seams | Finished when |
|---|---|---|
| A: server contract and packaging | [`stream.rs`](../../crates/plurxd/src/http/stream.rs), its existing tests; core playback/transcode helpers only if necessary | Conversion routes to HLS; progressive narrowing and DV muxing are correct together |
| B: Android execution | [`Models.kt`](../../clients/android/app/src/main/java/tv/plurx/app/data/Models.kt), [`PlayerScreen.kt`](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt), [`Controller.kt`](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt), [`PlaybackIntent.kt`](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackIntent.kt), [`SubtitlePolicy.kt`](../../clients/android/app/src/main/java/tv/plurx/app/player/SubtitlePolicy.kt), adjacent JVM tests, Android version metadata | All media attachment paths obey the same transport rule |
| Closeout in the same PR | This document, RCA, [`PLAYBACK.md`](../PLAYBACK.md), [`API.md`](../API.md), [`docs index`](../README.md), applicable validation catalog entries | Final behavior, focused commands and evidence are recorded without duplicate status documents |

Keep A atomic: do not ship `-strict unofficial` before protecting P5/P8-only
clients from preserved, unconverted P7. B can remain compatible with older
servers because its new wire property defaults to false. No server/APK
version handshake or coupled deployment is required.

## 4. Server contract — plan for the producer that will execute

### 4.1 Request-scoped conversion availability

Keep the core conversion rules in
[`playback/mod.rs`](../../crates/plurx-core/src/playback/mod.rs). Do not add
another DV profile classifier. In the HTTP planning layer, use a small shared
helper to derive request-scoped `RenderCaps` from the existing node facts:

| Request context | Conversion producer available to this request |
|---|---|
| Decision POST with actual v2 caps advertising HLS | Existing node conversion value |
| Decision POST with actual v2 caps without HLS | False for this request |
| Legacy decision query without an explicit transport document | Preserve current conversion eligibility; advertise required HLS if conversion is selected |
| Concrete progressive GET | False for this request, regardless of advertised transports |
| Concrete HLS session create | Existing HLS review/producer capability rules |

`RenderCaps` is copyable. A request-local copy with conversion unavailable
is sufficient; do not change the persisted switch or inflate
`DeviceProfile`. Preserve the distinction between actual v2 input and the
legacy query translated into a v2-shaped struct: the latter's empty
`transports` vector is not an explicit refusal of HLS. For actual v2 input,
an omitted/empty list does not advertise HLS.

Run the existing forced decision logic with these effective producer facts.
Keep audio selection, forced quality, learned codec limits and all other
policy inputs intact. Derive preservation, conversion, delivered range,
profile and reasons from the resulting decision together; do not simply
flip `convert_dolby_vision` after constructing an inconsistent verdict.

For the motivating compatible source/client, removing conversion from the
available operations should select the existing HDR10 strip path. If the
base is incompatible, stripping unavailable, or the display unsuitable,
retain the existing supported-transcode/refusal policy. Never manufacture
an HDR10 fallback for a non-compatible P5 source.

### 4.2 Decision response and HLS requirement

After the effective decision and audio adjustments, calculate:

```text
remux requires HLS = conversion selected OR existing sample-entry rule
```

Conversion must be checked independently of whether
`progressive_hevc_sample_entries` is present. Retain the existing
sample-entry check and its genuine unsupported-delivery error when no
advertised path can carry the output. A conversion request that was already
narrowed to a compatible progressive strip does not retain a stale
conversion-derived HLS requirement.

Keep `delivery.mode=remux`, `sessions_url`, audio selection, AAC choice and
preservation fields. `requires_hls` already exists on the server wire;
consume it correctly rather than add a second routing field. HLS packaging
with copied video is not a video transcode.

Audit delivery metadata and the index lookup against the **served video
identity**, using existing copy-option/identity helpers. Never report an
index for preserved P7 as an index for converted P8.1. A missing converting
index must not stop playback: fresh live copy HLS already converts (§6).

### 4.3 Progressive execution and truthful metadata

At `stream_mp4`, retain the requested decision for diagnostics, then derive
the served decision with progressive conversion unavailable. Do this before
registering the delivery/spawning FFmpeg; where playback-start metadata
depends on the recipe, calculate it before publishing that metadata too.
Do not otherwise redesign activity bookkeeping.

For a supported compatible-base fallback:

- Both served preservation and conversion are false.
- Existing stripping removes RPU/EL NALs and stale DV configuration.
- Output keeps the compatible base's actual HDR signaling and HEVC entry.
- Audio index, audio offset, start position and request ownership survive.
- Exposed delivery/status fields describe HDR10 with no delivered DV profile.

Apply this even if the caller advertised HLS but requested `stream.mp4`.
Do not redirect to a session-create operation or introduce a new 409 while
a compatible route exists. If the available filter cannot remove stale DV
metadata correctly, use existing unsupported-operation handling; do not
claim that deleting NALs alone necessarily removed container side data.

Keep source metadata separate from delivered metadata. A progressive binary
response does not retroactively replace an old JSON verdict in every client;
record that limitation instead of adding a cross-client protocol redesign.
Correct the existing server fields and Android's existing format/session
adoption paths, and verify them independently of picture output.

### 4.4 Native-DV progressive packaging

Extend `RemuxSpec`/`progressive_hevc_copy_args` only enough to carry the
structured source facts already used by `hevc_copy_tag_for_source` in
[`transcode/mod.rs`](../../crates/plurx-core/src/transcode/mod.rs).
Use that same tag choice in `progressive_hevc_output_tag` so admission and
emission cannot disagree.

| Served copy | Ordinary sample entry | Muxer strictness |
|---|---|---|
| Preserved compatible P8.1 | Existing source-aware `hvc1` choice | `-strict unofficial` |
| Preserved non-compatible P5 | Existing source-aware `dvh1` choice | `-strict unofficial` |
| Stripped compatible base | Ordinary HEVC choice | No DV-specific strictness requirement |
| Minimal-hvcC / in-band parameter-set promotion | Retain existing `dvhe` when preserving, `hev1` otherwise | Unofficial when preserving DV |

Apply strictness to **all served preserved-DV copies**, after endpoint
narrowing. Preserve the existing promotion branch and its filter behavior;
do not globally substitute `hvc1` for in-band parameter-set output. Preserve
RPU metadata on native-DV copies. Add no conversion code to progressive.

### 4.5 Small, useful diagnostics

Use structured fields on existing decision/delivery logs: input capability
version, normalized DV profiles/transports, requested preserve/convert,
served preserve/convert, served dynamic range/profile, selected transport,
sample entry, and narrowing reason. Reuse existing file/session/request
correlation identifiers. Avoid logging each fragment.

Log output option facts or an explicitly sanitized output-options subset.
Never log the complete FFmpeg argv, input URLs, bearer tokens, capability
URLs, or private paths merely to prove `-strict` was selected. Do not build
a new telemetry service or a playback-enable predicate from these fields.

## 5. Android contract — transport survives lifecycle changes

### 5.1 Wire and immutable recipe

Add `requires_hls: Boolean = false` to `Delivery`. Carry it through the
screen's plan mapping, `PlanLike`, and `PlaybackMediaRecipe` as
`requiresHls`. Include the requirement in media-equivalence comparison so
a changed transport requirement cannot be treated as a native selection.
Defaults preserve omitted-field compatibility with old servers and tests.

Centralize desired transport selection in one small pure function/property:

```text
existing burn/forced-transcode route     → HLS transcode
existing native-subtitle session route  → existing HLS route
plan transport + remux + requiresHls    → HLS copy
plan transport + remux                 → progressive remux
plan transport + direct                → direct
other existing transcode route         → HLS transcode
```

Retain the meaning of `SubtitleDelivery.usesPlanTransport`. Subtitles Off
does not cancel a plan's HLS requirement. Bitmap overlay follows the plan's
transport without being mistaken for subtitle burn.

Distinguish desired transport from the transport of the attached player.
The existing `directTransport`/`progressiveTransport` predicates must use
the actual attachment when one exists, not infer progressive solely from
`mode == remux`. A prepared HLS successor is HLS even if its original mode
was remux. Record that fact at attachment/commit using a small transport
value alongside existing recipe ownership; do not rewrite ownership or
acknowledgement sequencing. A pending successor must not change the
incumbent's timeline interpretation before commit.

### 5.2 Execute and retain the route

Use the shared rule at initial open, resume/reopen, `executeSeek`,
`restartAt`, audio change/offset change, subtitle change/Off, recovery, and
prepared replacement attachment. Audit every progressive-URL construction
and every media attachment, including the prepared-player commit that
bypasses `attachRecipe`.

Reuse/extract the existing copy-session body construction from
`SubtitlePolicy.kt`. Required-HLS remux sends the existing caps document,
selected audio, AAC choice, audio offset, preservation request and start
position. Preserve native subtitle fields only when selected. The server
derives conversion: no client-authoritative convert override, fake subtitle
selection or forced video transcode is added.

Use existing HLS seek/control, session cleanup, readiness and recovery
mechanisms. Preserve their media-origin accounting; progressive offset math
must not be applied to an HLS attachment. Reuse `adoptSessionDelivery` so
the actual served grade/profile supersedes the initial plan. Preserve the
existing subtitle-burn HDR restrictions and Original-quality semantics.

Bump Android `versionCode` above both current merge-target and installed
builds using the existing mobile-version rules. Build 107 is incident
evidence, not a permanent baseline for choosing the next code. No Apple
build-number bump or workspace release-version change is implied.

## 6. Reuse the converter — live first, indexed VOD second

The converter already exists in the copy segmenter and fragment index
paths. Retain their converted init record, per-fragment RPU rewrite, EL
removal and conversion identity. Relevant seams are
[`copyseg.rs`](../../crates/plurxd/src/copyseg.rs),
[`fragindex.rs`](../../crates/plurxd/src/fragindex.rs),
[`transcode.rs`](../../crates/plurxd/src/transcode.rs), and HLS
`review_client_plan`/`apply_plan_review` in
[`hls.rs`](../../crates/plurxd/src/http/hls.rs).

The primary exercise is a **fresh copy-HLS session with no converting
index**. Do not require background indexing to finish or queue a library
conversion before this path can be used. Indexed VOD is a separate, focused
case against a short fixture, not a second full-length device campaign.
Retain existing asked/served narrowing on older/takeover paths and verify
that their response reports what they actually serve.

## 7. Focused acceptance — finite, observable, repeatable

Add tests at the existing policy/builder and Android recipe seams. Prefer
table-driven cases to many almost-identical tests. Tests must exercise
observable choices or emitted records, not merely restate a helper body.

| Case | Required evidence |
|---|---|
| P7, P8-capable v2 client, HLS, sample entries absent | Remux, conversion and preservation true, `requires_hls=true` |
| Same client with progressive-only/empty v2 transports | Admissible HDR strip, no delivered DV profile; no transport-only 409 |
| Legacy decision and legacy HLS create | Unknown transport is not globally treated as no HLS; session review remains compatible |
| Progressive request from P8-only client, with or without claimed HLS | Conversion is unavailable; clean compatible strip, consistent served metadata, original audio/start/offset |
| Conversion explicitly disabled | Existing compatible fallback and saved preference respected |
| Native P5/P8; ordinary and promotion branches | Correct tag, retained DV configuration/RPUs; admission agrees with output |
| Incompatible base, unsupported strip, or unsuitable display | Existing supported transcode/refusal; no fabricated HDR10 success |
| Android omitted wire field | Previous routing remains compatible |
| Required-HLS remux: start, seek, reopen, audio/offset, subtitle On→Off, recovery | Same required transport, correct request body and timeline; no accidental progressive URL |
| Prepared HLS handoff with a remux plan | Incumbent transport remains authoritative until commit; successor is recorded as HLS |
| Ordinary direct/remux and burn/transcode controls | No blanket HLS migration or bypass of existing burn policy |
| Fresh live conversion and short indexed VOD conversion | P8.1 output record/RPUs, EL absent, matching converted identity and served metadata |

Use small existing authorized media fixtures or generated fixtures for
FFmpeg/container checks. Parse the complete init/`moov` and a complete media
fragment. An argv assertion is necessary for a builder regression but is
not enough for mux correctness; an ASCII search in the first 4096 bytes is
not an absence test. Verify the converted record describes P8 with
compatibility 1 and no EL, and inspect the fragment's RPU/NAL structure.

Run one controlled Lenovo pass when the tablet is available: file `5418`
through fresh live conversion, a seek, audio change, subtitle On then Off,
and recovery/reopen; then one native P8 progressive control. Retain matching
decision/session identifiers, init/fragment evidence, and Media3/Android
codec format. Expected device selection is `c2.dolby.decoder.hevc` with
`video/dolby-vision`; do not force that decoder name in product code.
Compare startup/stability with the existing baseline and investigate a
material regression without inventing a new runtime threshold.

Native P5 and indexed VOD can use fixture-level checks; no full matrix on
every physical client is required. Codec/format evidence supports decoding,
not a measured panel-output claim. Do not interrupt unrelated viewing for
captures. A capture starts delivery and can update playback activity.

## 8. Build, review and merge — use the current fast lane

Follow the dated correction at the top of
[`DEVELOPMENT_PIPELINE.md`](../DEVELOPMENT_PIPELINE.md), not its superseded
automatic full-qualification/post-merge sections. This is an ordinary
independent correction on one PR, not an effort promotion. No CI workflow
changes are part of this repair.

1. Verify `rustc --version` is the repository-pinned **1.97.1** before Rust
   edits. If unavailable locally, establish the source-only
   [compile loop](../ci/AGENT-COMPILE-LOOP.md): `git archive` committed source,
   transfer no `.git` or credentials, reuse the warm build directory.
2. Implement A and B with focused regressions. Commit normally; preserve
   existing hooks and do not use CI to discover compilation errors. Hooks
   do not substitute for the explicit tests below.
3. Open the single PR as **draft**. Obtain exactly one adversarial code
   review of the complete change, address it, and rerun checks affected by
   fixes. Fable's design review is input, not the implementation review.
   Do not introduce a routine second review round.
4. Verify the final intended branch after any base integration. Mark ready
   once it is ready for the existing fast lane; there is **no `fast-lane`
   label**. Draft PRs allocate no jobs; returning to draft cancels the lane.
5. Merge only with the current candidate's **Main promotion gate** passing.
   The lane selects affected checks, including Rust/Windows and Android
   builds as applicable. It does not replace local JVM/behavior tests.

Run the following on the affected source with the pinned toolchain; include
core targets if the patch touches shared helpers:

```bash
rustc --version
cargo fmt --all -- --check
cargo check -p plurxd -p plurx-core --all-targets
cargo clippy -p plurxd -p plurx-core --all-targets -- -D warnings
```

Give the new Rust regression cases a common `android_dv_delivery` name
prefix and run them explicitly; these are planned tests, not existing
results. Also run the existing touched-module DV/copy tests, including live
and index converter tests. Record actual test names and nonzero test counts
in the PR; a filter that executes zero tests is not evidence.

```bash
cargo test -p plurxd android_dv_delivery
cargo test -p plurx-core android_dv_delivery
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

Omit the core filtered command if no new core tests were needed. On the
Android build host, from `clients/android`, run focused recipe/serialization
tests while editing, then one final pass:

```bash
./gradlew testDebugUnitTest :app:assembleDebug :app:lintDebug
```

Use the existing Android build environment if the checkout host lacks the
JDK/SDK. The fast lane's debug compilation is not a substitute for the JVM
tests. Run the applicable validation catalog/static checks; leave unrelated
catalog entries and pipelines alone.

Full CI and effort compiler/evidence jobs are manual under the current
workflow. Do not add a full release matrix, repeated qualification cycles,
per-commit device campaigns, or a new receipt system to this PR. Broaden a
check only when a failure, changed dependency or concrete unresolved risk
justifies it. If `main` moves and the candidate is integrated again, rerun
the affected compiler/tests on that tree rather than cite an older snapshot.

## 9. Delivery and closeout — a short result record, then stop

Merge does **not** build or deploy an image. Use the existing explicit build
and deployment process when implementation is ready for acceptance. Server
A is safe with old Android through progressive fallback; new Android is
compatible with an old server through the omitted-field default, but the
old server cannot provide the repaired routing contract. Record the actual
server revision/image and APK version used for the Lenovo pass.

The existing conversion switch is the optional operator control for
conversion. It is not a rollback for native-DV packaging; a packaging
regression needs a corrected build or revert of the coherent server change.
Do not add automatic rollback/disable heuristics based on diagnostics.

Finish by replacing this document's unbuilt status with the result, and
record one compact block containing PR/commit, build versions, commands and
test counts, media/device evidence, and any specific remaining limitation.
Update the RCA and maintained API/playback references to the shipped
behavior. Do not create a separate milestone tracker or periodic status
automation for this repair.

The repair is complete when the server and Android contracts pass their
focused checks, the single code review is addressed, the current fast lane
passes, and the controlled Lenovo run verifies converted and native DV
delivery. If hardware access is unavailable, state that device acceptance
is pending without calling it proved or creating a software gate. Keep any
unrelated discovery separate and close this work once its contract is met.
