# Library channel playback repair — fix the producer, prove the tune, finish

**Status:** implementation complete; compiler checks passing; draft PR preparation in progress · **Written:** 2026-09-10 ·
**Executes:** Paul's request to repair channel playback without a prolonged
development programme, software feature gates, or restoring the old CI fan-out ·
**Investigated source:** `origin/main` at `9a857b2e5` · **Implementation base:** `94b46515`

Read §1–§4, establish the compiler loop, then execute §5 in order. This is one
corrective change with one main-bound pull request. It does not reopen the
Library channels feature build. If the short route in §4 cannot deliver the
required bytes, report that specific result at the one scope checkpoint;
do not quietly turn this into a general MP4 engine rewrite.

Companion to [the channel status](LIBRARY-CHANNELS-STATUS.md)
(the feature and its user contracts), [PLAYBACK.md](../PLAYBACK.md)
(the shared delivery paths), [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md)
(the current merge process), and [VALIDATION.md](../VALIDATION.md)
(the evidence catalogue). This document answers: *what is the smallest
complete repair, how do we prove it, and when do we stop?*

## 1. Deliver a working channel tune within a bounded change

An enabled Library channel must start its scheduled programme at the resolved
offset using the existing playback system. The reproduced HEVC source must
reach decoded video and audible audio. Following must still move to the next
programme; Watch from start must remain ordinary personal playback.

**Delivery shape:** one owner, one branch, one draft PR into `main`, exactly
one adversarial review, then the existing fast lane. Use ordinary commits on
`codex/library-channel-playback-repair`; do not create an effort tree or
separate PRs for every helper. Follow newer explicit branch instructions if
the implementation session supplies them.

**Planning budget:** aim for two focused engineering days, not a guarantee
about runner or deployment availability. Spend at most about 90 minutes on
the bounded muxer experiment in §4, excluding compiler setup. Once the
reproduced case passes, spend the remaining time on integration, focused
regressions, and promotion. Do not keep exploring alternative architectures.
If the work exceeds that envelope, state the concrete remaining blocker and
the smallest additional change before expanding scope.

### 1.1 Availability belongs to the user

The repair is ordinary production behavior. Add no experimental flag,
environment opt-in, per-title allowlist, device certification requirement,
rollout cohort, feature licence, or readiness threshold. Do not require a
fully indexed library before a user can tune a channel.

Prefer no settings change. If an enable/disable control is needed, reuse the
existing Library channels control in Settings → Developer. Requirements and
whether they are met may be shown there as advisory facts. They must not
override the user's enable choice, hide navigation, disable authoring, or
become a second enable condition. Do not add a separate fallback toggle.

Ordinary authentication, media permissions, finite resource admission, and
checking that emitted media is valid remain part of executing a request.
These checks must not become catalogue-wide feature eligibility rules. A
missing file can fail that playback attempt; it cannot disable channels.
When a requested delivery recipe cannot produce usable bytes, use the
existing compatible recovery behavior or return its precise request error.
An unavailable advisory measurement is not a reason to reject a tune.

### 1.2 Work deliberately outside this change

- No channel scheduling, authoring, guide-layout, tuner, or watch-history
  redesign. Those are not the cause of the reproduced producer failure.
- No general support programme for arbitrary MP4 configuration changes,
  mixed codecs in one track, new HDR formats, or new transcode policies.
- No fleet-wide rescan, index rebuild, cache purge, or re-encoding campaign.
  A targeted correction may be needed for the source in §2.3.
- No new retry controller, background daemon, database migration, metrics
  dashboard, benchmark programme, or client capability protocol.
- No blanket use of the older system FFmpeg, and no dependency downgrade.
  A different muxer output is a useful diagnostic, not a deployment fix.
- No CI workflow redesign, restored full pre-merge suite, mandatory device
  matrix, pre-commit hook, review panel, or second adversarial review.

## 2. Evidence — distinguish confirmed failures from open questions

### 2.1 The screenshot's failure is reproduced on the deployed build

At 15:26:35 EDT / 19:26:35 UTC on 2026-09-10, nynuc build
`v0.3.0-1996-g9a857b2e` attempted file **5310**, *Leave the World Behind*, at
**6275.560 seconds**. The source is HEVC in Matroska with E-AC-3 audio. The
recorded sequence was:

```text
 channel resolve → finite-media playback request
                           │
                    vod_index_pending
                           │
                 temporary streaming recovery
                           │
       HEVC copy + hevc_mp4toannexb,extract_extradata
                           │
                two hvc1 descriptions in stsd
                           │
             copy reader returns Unsupported
                           │
      reconstruction requirement prevented retry creation
                           │
       producer failed before publication: Unsupported
```

This is not evidence of an authoring failure or a browser extension blocking
the request. The server returned the terminal producer failure before a
playlist was available. Later generic browser advice about blockers is
misleading for this attempt.

The deployed `/usr/lib/jellyfin-ffmpeg/ffmpeg`, using the production copy
filters and offset, produced two distinct `hvc1` entries in a bounded
12-second remote reproduction. Their `hvcC` boxes were 255 and 147 bytes.
Their VPS and SPS matched; their PPS differed, including 8-byte versus
9-byte NAL payloads. These are not duplicate entries that can be deleted
based on equal codec names. The observed fragments used description 1 via
the `trex` default; no explicit `tfhd` override appeared in those 12 seconds.
That observation does not prove the rest of the film never changes index.

The container's default `/usr/bin/ffmpeg` reported version 5.1.9 and emitted
one description in the initial shorter reproduction. It also rejected the
production `readrate_initial_burst` option. Reproduce with the daemon's
configured binary and record its version; `ffmpeg` from `PATH` is insufficient.
The diagnostic returned box metadata and hashes only, without exporting
media. No source file or production configuration was changed.

### 2.2 Why the previous corrections did not finish the job

| Correction | What it addressed | What it did not establish |
|---|---|---|
| Collection routing, PR #234, merge `29d97094` | Collection URL compatibility and channel authoring/list access | Successful producer startup |
| Multi-entry HEVC, PR #238, commits `c79abcddd` and `b718bf581`, merge `9a857b2e5` | Independent decoder-description validation and the `Unsupported` classification | That this source receives an executable fallback or produces playable media |

In [session startup](../../crates/plurxd/src/transcode.rs), the inspected
eligibility expression is:

```rust
let retry = (segmenting
    && !video_options.promotes_parameter_sets()
    && !video_options.converts_dolby_vision())
```

The reproduced request sets `promotes_parameter_sets()`. Therefore its
`Unsupported` result has no installed retry to select. The guard has a real
reason: the legacy argument builder does not currently carry the complete
normalization choice, and that path does not perform Plurx's post-mux surgery.
Deleting the condition by itself is not the repair.

The new reader regression duplicates an identical HEVC entry and asserts
`Outcome::Unsupported` with no published playlist. The actor test supplies
its own retry recipe. Neither exercises production recipe construction for
a reconstruction-required source. Keep useful tests, but add the missing
connection rather than more classification-only assertions.

### 2.3 A later probe mismatch is a second, unresolved observation

At 19:28:14 UTC the same title was rejected with
`vod_source_rescan_required`. A separate attempt on *Nosferatu*, file 70,
also reported a probe mismatch. The logs do not identify which probe fields
differed or prove that either media file changed.

[The probe comparison](../../crates/plurxd/src/ffmpeg.rs) removes the filename
and tolerates omissions of three optional fields, then compares the remaining
JSON. Capture the actual differing field paths before deciding whether a
targeted rescan or a narrow comparison correction is required. Do not treat
this as permission to ignore source identity. Forced transcode has not been
qualified as a workaround while this refusal remains unexplained.

## 3. Work at the existing seams

These anchors were inspected at `9a857b2e5`. Re-verify against current `main`
before implementing; this document was authored in an older checkout with
unrelated uncommitted work. Port only the relevant document and implementation
changes into an isolated checkout. Do not reset or sweep in that other work.

| Seam | Current interface or responsibility | Repair responsibility |
|---|---|---|
| [Copy arguments](../../crates/plurx-core/src/transcode/mod.rs) | `CopyVideoOptions::from_probe`, `copy_video_args`, `hls_copy_args_with_sequence` | Preserve normalization in a compatible muxer recipe |
| [Startup and execution](../../crates/plurxd/src/transcode.rs) | `PrepublicationCopyRetry::build`, `execute_prepublication_copy_retry`, frozen presentation | Build a real retry before admission; retain exact attempt ownership |
| [Copy reader](../../crates/plurxd/src/copyseg.rs) | `run`, `hevc_promotion_failure` | Report actual unsupported structure; do not make retry decisions here |
| [Control actor](../../crates/plurxd/src/playback_control.rs) | `InitialProducerPolicy::copy`, `ValidatedRetryRecipe` | Retain one retry and publication/retirement fences |
| [MP4 model](../../crates/plurx-core/src/fmp4.rs) | `validate_hevc_sample_entries`, `promote_from`, `merge` | Reuse structural checks; expand description handling only if §4 requires it |
| [VOD init identity](../../crates/plurxd/src/renditiondir.rs) | `InitIdentity::establish`, `served_init_for` | Keep generated bytes and stored identity consistent |
| [VOD generation](../../crates/plurxd/src/vodgen.rs) | Planned segments, immutable published presentation | Never substitute rolling HLS under an already published VOD playlist |
| [Source comparison](../../crates/plurxd/src/ffmpeg.rs) | `probes_describe_same_input(stored, held)` | Explain and resolve the observed mismatch narrowly |

The actor remains the only retry authority. Do not teach the HTTP handler,
channel scheduler, copy reader, or browser to launch extra attempts.

## 4. Choose the smallest complete producer repair once

The initial investigation recommended shared sample-description support.
That remains the broader architectural answer, but Paul's time constraint
changes the implementation order: first establish whether the existing
FFmpeg HLS muxer can carry this source correctly when given its required
normalization. A muxer that already understands descriptions may avoid a
large parser/merger change. This has **not** yet been demonstrated.

### 4.1 Preferred route — preserve normalization in the existing retry

Run one bounded experiment, using the daemon's configured FFmpeg and the
same source, seek, video/audio selection, and filter chain as production.
Generate a short fMP4 HLS presentation with the native FFmpeg HLS muxer into
isolated scratch, retaining `hevc_mp4toannexb,extract_extradata`. Do not use
the existing unmodified legacy recipe as the candidate: losing those filters
would repeat the bug the eligibility guard prevents.

Inspect the resulting init and fragments, then decode the candidate. Check
complete HEVC parameter sets, description references, expected video/audio
tracks, source offset, timestamps, and first decoded frames. If the init
contains multiple descriptions, verify that the muxer and decoder use their
indexes correctly; do not equate parser acceptance with playback. For a
candidate intended to preserve HDR/DV, inspect the actual color/DV signaling
as well. SDR evidence from file 5310 is not HDR qualification.

If this works without adding a post-mux repair engine:

1. Pass the frozen `CopyVideoOptions` through HLS argument construction so
   the retry retains the required filters. Reuse the existing argument
   helpers; do not string-patch an argv or duplicate an entire builder.
2. Install that immutable, presentation-compatible recipe for the supported
   reconstruction case. Replace the blanket promotion exclusion with actual
   recipe capability. Keep exclusion of unsupported DV conversion: the
   native muxer cannot stand in for Plurx's RPU converter.
3. Validate emitted decoder configuration using the existing publication
   machinery before media handoff. Add only the missing check at that seam;
   do not invent a readiness service. A malformed output fails its request.
4. Preserve source offset, audio choice, achieved media origin, advertised
   codecs, dynamic range, and recipe fingerprint. A materially different
   presentation cannot be substituted under an already frozen identity.
5. Use the existing one-shot retry executor and scratch cleanup. No second
   copy retry, policy reread, or new process survives cancellation/retirement.

This is the preferred complete repair if its generated bytes and integrated
playback pass. Do not also implement general sample-description support in
the same PR for completeness.

### 4.2 The single scope checkpoint if the muxer route fails

Record the failing command shape, binary version, box/description facts,
and decoder error. Stop trying filter combinations after the bounded
experiment; state what the muxer still cannot preserve. Propose the narrower
shared-reader change below with its added cost before starting it. This is a
scope decision, not another code review or an implementation approval ritual.

The alternative must be description-aware throughout, not merely tolerant:

- Keep ordered sample descriptions per track. Resolve the 1-based index from
  `tfhd.sample_description_index`, otherwise the track's
  `trex.default_sample_description_index`; check bounds and truncated fields.
- Read codec configuration from the selected description rather than the
  last entry visited. Do not merge different PPS records or drop a description
  because its codec string matches another.
- Associate promotion inputs with the relevant track and description.
  Preserve untouched entries and deterministic VOD regeneration; do not
  promote one fragment's data indiscriminately into every configuration.
- Preserve description identity through fragment rewriting and `merge`.
  Today the parser skips the index and the merger emits a minimal `tfhd`.
  Relaxing init rejection while leaving those behaviors is incomplete.
- Keep each emitted track fragment associated with one description. A mixed
  description input requires faithful fragment separation or existing typed
  recovery; never combine samples under a guessed default. Account for the
  fixed VOD segment plan rather than changing its published durations.

Limit this alternative to the HEVC configuration shapes needed by the
reproduction and their direct correctness cases. Do not add codecs, rewrite
the index architecture, or weaken immutable VOD identity. If that boundary
cannot hold, return with the concrete limitation instead of building an
unbounded media framework.

## 5. Execute three work packages, then stop

### 5.1 Reproduce, select the route, and identify the probe difference

Establish the repository-pinned compiler before editing Rust, following
[the compile loop](../ci/AGENT-COMPILE-LOOP.md). Record the actual `rustc`
version. When compiling elsewhere, transfer committed source with
`git archive`, never `.git` or credentials, and keep `target/` warm.

Run §4.1 and record one route decision in this document. In the same bounded
diagnostic pass, compare stored and held-source probes for file 5310. Use
the exact configured probe binary and comparable probe options. Record field
paths and relevant values, with credentials and unrelated metadata removed.

If the source actually changed, use the existing targeted rescan mechanism
for that source and verify the replacement probe/index identity. If stable
media yields a reporting difference, fix only the demonstrated normalization
case and add a changed-media counterexample. Do not add an ever-growing
ignore list, compare only size/mtime, or blindly accept a new probe. If the
mismatch has disappeared, record that limit; do not invent a source fix.

**Acceptance:** exact source/build/binary/offset recorded; the native-muxer
candidate either decodes or has a concrete refusal; the probe mismatch has a
field-level explanation or is explicitly marked unresolved. No bulk repair.

### 5.2 Wire the selected repair into real session startup

Implement only the chosen §4 route and any evidenced probe correction.
Keep channel purpose, authorization, history isolation, finite-media intent,
and programme transition behavior unchanged. Exercise production recipe
construction; do not hand-install a retry in the integration test.

At the existing diagnostic surface, make these facts recoverable: build,
session/attempt, file ID, requested offset, VOD refusal, producer route,
description count, retry availability and why it was unavailable, and the
selected actor decision. Extend an existing structured event rather than
adding repetitive per-fragment logs. The public error should retain a useful
cause or correlation identity instead of only `Unsupported`.

If the existing web error path replaces a server terminal refusal with
generic blocker advice, make that one narrow correction. Do not redesign
player recovery or add native-client work unless the reproduced failure
requires it. No client changes are otherwise expected.

**Acceptance:** the production startup path for a promotion-required source
can execute the selected compatible recipe and publish playable media;
retirement still prevents a successor, and a failed retry does not retry
again. A pending index is exercised, not bypassed by preparing every file.

### 5.3 Prove the tune and close the record

Use the small evidence set in §6. Record the tested revision, binary version,
scenario, and result here. Repeat affected checks only after a change that
could invalidate them. Do not spend a day rerunning already-green cases.

Promote through §7. After an explicitly deployed build, perform the actual
channel tune and transition checks. Merge does not deploy the fleet under
the current workflow. Use the established explicit build/deploy process;
do not create a release tag just to obtain a test image.

**Acceptance:** the exact observed failure is absent, video and audio play,
the next programme starts, and any remaining independent failure has its
own evidence and issue. Do not call the task complete from compilation or
a successful `POST /sessions` response alone.

## 6. Focused evidence — small enough to run, strong enough to matter

These are author-run implementation checks and deployment acceptance. They
are not new CI jobs, required status checks, or additions to the Main
promotion gate. Broader suites remain in the separately dispatched sweep.

| Check | Required observation |
|---|---|
| Realistic HEVC fixture | Two nonidentical descriptions, different PPS, complete configuration, explicit/default indexes; use generated or authorized fixture media, not a copied private film |
| Startup integration | Real recipe builder receives promotion-required metadata and an unsupported copy shape; a compatible successor publishes usable init and media within the existing startup deadline |
| Decode evidence | Candidate init plus segments decode to frames with advancing timestamps and audio; an HTTP 200 or playlist file alone is insufficient |
| Negative and ownership cases | Incomplete configuration is not published; invalid indexes are handled where applicable; cancellation or retirement cannot spawn another child; retry failure terminates after the existing single retry |
| Ordinary copy control | One existing single-description HEVC fixture and one H.264 fixture retain their normal path and output |
| Probe comparison, if changed | The measured same-source reporting difference passes; changed geometry, selected stream, or other relevant media facts still fail |
| Channel acceptance | File 5310 at the reproduced offset and one current scheduled offset play; one programme boundary advances; Watch from start and Return to channel preserve existing behavior |
| Prepared VOD control | A title with an existing usable index still follows its immutable VOD plan; do not silently substitute an EVENT presentation |

Use the existing Rust test harness and media-fixture helpers. New tests should
share a `channel_playback_repair` name prefix so focused commands are obvious:

```bash
# Proposed prefix for new tests; confirm nonzero test counts in both outputs.
cargo +1.97.1 test -p plurx-core channel_playback_repair -- --nocapture
cargo +1.97.1 test -p plurxd --bin plurxd channel_playback_repair -- --nocapture
```

The test environment must use the project's configured production-compatible
FFmpeg/ffprobe paths. Pin the container or record the exact binaries; do not
silently fall back to the host defaults. Choose small synthetic fixtures for
repeatable tests and use the real title only for the bounded acceptance.

Use one browser on the affected server for channel acceptance, watching at
least 30 seconds of the reproduced tune and one programme transition. Arrange
the transition with an existing short test schedule or a programme near its
end; do not wait through a film. Record time to first frame against the
existing startup deadline, not a new arbitrary performance threshold.
Shared server changes do not create a mandatory all-device campaign.

## 7. Promote using the current velocity-oriented workflow

[AGENTS.md](../../AGENTS.md) and
[the development pipeline](../DEVELOPMENT_PIPELINE.md) govern this change.
Do not restore older process requirements from historical plans.

1. Start from current `main` in an isolated checkout. Open the corrective PR
   as a draft. Drafts allocate no Forgejo jobs.
2. Compile while editing, with the pinned toolchain. Before ready, run
   formatting, workspace checks, and Clippy against the exact branch that
   will be promoted. If the base moves, compile the integrated source again.
3. Complete corrective evidence, path ownership, and documentation in the
   implementation commits. New/moved docs receive an index row. Update
   [the catalogue](../../validation/points.toml), using existing owners and
   checks rather than creating another CI lane.
4. Request exactly **one** adversarial agent review of the complete draft.
   Focus it on normalization preservation, actual retry availability,
   publication identity, source comparison, and attempt ownership. Address
   every finding and verify the changes as author. No re-review or panel.
5. Mark ready only after findings and bookkeeping are addressed, then apply
   `fast-lane`. Merge only with a green current-head `Main promotion gate`.
   If returning to draft, remove `fast-lane` first.
6. Deploy explicitly through the existing process and perform §5.3. Keep
   deployment acceptance distinct from merge status in the evidence record.

The fast lane contains policy, deterministic static contracts, and affected
compilation. Unit, integration, browser, playback, simulator, emulator,
recovery, package, and smoke suites remain outside the pre-merge contract.
Do not add the §6 matrix as workflow dependencies or wait for a full sweep
to permit merging. The focused checks establish whether the fix works;
they do not replace the one review or current-head compiler evidence.

Useful compiler and bookkeeping commands, subject to the current pipeline:

```bash
rustup run 1.97.1 rustc --version
cargo +1.97.1 fmt --all -- --check
cargo +1.97.1 check --workspace --locked --all-targets
cargo +1.97.1 clippy --workspace --locked --all-targets -- -D warnings
make history-check
make operations-check
make validation-lint
git diff --check
```

Corrective commits need current evidence under `validation/regressions.d/`
unless their changed tests are direct evidence. Do not create repair-only
paperwork commits. Bump Apple/Android counters only if a shipped change
touches that platform; a workspace release must align both platforms and
marketing versions. This server repair does not request a version release.
Do not install the pre-commit hook or change the full-sweep schedule.

## 8. Finish line and the one evidence record

Update the table below in this document; do not create a separate status
programme. The investigation is complete, but none of the repair checks may
be marked passed until they have actually run against the implemented change.

| Item | Result / revision / remaining action |
|---|---|
| Production failure and metadata reproduction | Confirmed at `9a857b2e5`, 2026-09-10; §2 |
| Normalization-preserving native HLS experiment | Passed 2026-09-10 on nynuc, deployed `v0.3.0-1996-g9a857b2e`; configured Jellyfin FFmpeg 8.1.2, file 5310, seek 6275.560 s; 12 s native HLS, two distinct complete HEVC descriptions, 328 decoded frames, AAC 48 kHz / six channels with non-silent RMS; scratch media deleted |
| Selected implementation route and reason | Preferred §4.1: retain frozen normalization in native HLS retry; candidate decoded with both original descriptions and trex default index 1. SDR BT.709 only; no HDR/DV qualification claimed |
| Stored/live probe field comparison | Captured from authoritative Hiqlite SQLite file, read-only. File 5310 differs only by six omitted audio report fields: dmix_mode, loro_cmixlev, loro_surmixlev, ltrt_cmixlev, ltrt_surmixlev, mime_codec_string. File 70 has additional version-reporting omissions. No source or stored probe changed |
| Integrated startup and decode checks | Focused current-head run passed after review: plurx-core 2/2, plurxd 3/3; production Jellyfin FFmpeg diagnostic decoded 328 frames plus non-silent AAC. Browser playback-control suite passed after aligning its full-open harness with the shipped channel state |
| Ownership and ordinary-playback controls | Actor regression drove a production `Unsupported` classification to the exact frozen one-shot retry; first-media validation accepted the encoder-produced distinct-PPS init and fragment reference and rejected malformed init bytes |
| Pinned compilation and bookkeeping | `rustup run 1.97.1 cargo check --workspace --locked --all-targets` passed after the implementation on `94b46515` in 37 s; Rust formatted; path ownership updated. Clippy and deterministic bookkeeping remain before review |
| Exactly one draft adversarial review; findings addressed | Completed on draft PR #240 at `249ead8c`; four findings addressed in `35b9d4b4`: promised HEVC and description-index validation, actor transition coverage, encoder-produced distinct PPS, and audio-only probe normalization scope. No second review requested |
| Current-head Main promotion gate and merge | Repair pending. Prerequisite CI trigger PR #239 merged as `94b46515` under Paul’s explicit syntax/lint-only exception; full runtime sweeps now manual/tag-only and fast lane remains separate |
| Explicit deployment revision | Pending |
| Real channel tune, audio, transition, restart/return | Not run on a repaired build |

The task ends when the selected repair is merged, explicitly deployed, and
the bounded channel acceptance passes, with any actual probe correction
included or a clearly separate remaining issue documented. An unresolved
probe refusal that still prevents the target channel from playing means
playback is not accepted yet; do not relabel it as success.

Do not extend acceptance into every title or every client. Additional
independent defects get their own issues and ordinary correction process.
If the deployed change regresses playback, roll back the explicit deployment
using the established process and retain the failing evidence. Do not hide
the regression by automatically disabling Library channels or introducing
a new feature flag.

## 9. Execution handoff — 2026-09-10

Paul requested a switch from Astra to Sol after the diagnostic pass. Work continued
in the independent clone `/private/tmp/plurx-channel-repair-20260910`, branch
`codex/library-channel-playback-repair`, based on `94b46515`. The user’s original
checkout remains untouched. The repair now preserves `CopyVideoOptions` in the
native-HLS retry, validates emitted init bytes at first-media publication,
normalizes only the six measured optional audio report fields, and preserves a
typed terminal server refusal in the browser stall diagnosis.

**Confirmed user decisions:** Focused repair tests run after the single
adversarial review, alongside the fast lane. Compile while implementing.
Paul explicitly authorized the separate quick CI correction to merge after
syntax/lint and requested cancellation of active tests; Forgejo reported no
active runs at both inspections. PR #239 is merged; no repair PR exists yet.
The original checkout’s unfinished CI work was inspected read-only; only a
small trigger correction was independently implemented, preserving pinned
Actions and the existing fast lane. Old workflow contract tests may need
narrow alignment during the repair’s fast lane; no runtime suites were run.

**Diagnostic artifacts (local scratch):**
`/private/tmp/plurx-channel-diagnostic.py` and `.log` contain the bounded muxer
experiment and output; `/private/tmp/plurx-probe-diagnostic.py` and `.log`
contain the read-only probe comparison. The scripts were streamed through SSH,
not installed on nynuc. Media remained on nynuc and its experiment directory
was deleted in `finally`. No production setting or probe was changed.

**Implementation seams:** The native-HLS recipe now receives the already frozen
`CopyVideoOptions`; parameter-set promotion is supported while in-process Dolby
Vision conversion remains excluded. `authorize_response_publication` performs
a bounded read of the immutable init and calls
`fmp4::validate_hevc_sample_entries` before the actor’s first-media handoff.
Attempt, replacement, retirement, and request deadlines fence that read. The
fixture now carries two complete, nonidentical descriptions with a distinct PPS,
and the startup regression checks the actual retry constructor and argv.

**Probe follow-up:** File 5310’s exact six optional audio reporting omissions
are normalized symmetrically: a value still differs when both probes report it,
and geometry and stream identity remain significant. File 70’s wider FFmpeg
schema drift is deliberately outside this correction and will be recorded as a
separate issue. No replicated database row is written directly.

**Web correction:** `stallDiagnose` now retains a current nonretryable
`STREAM_FAILURE` as the diagnosis and skips the misleading generic network
probe. Existing generation fences still prevent a stale result from labeling a
replacement playback.

**Tools and access:** SSH to nynuc works with the user-specified deployment
key. Forgejo helper `/private/tmp/plurx-forgejo.py` reads the token from the
user-specified file without printing it. Forgejo’s run listing ignored the
requested limit and returned all runs; filter summaries before printing.
The helper’s API base is `http://192.168.4.7:3000/api/v1`. Rust commands must
use `rustup run 1.97.1 cargo`; the host’s plain `rustc` is Homebrew 1.98.0.
A warm local `target/` exists. No pre-commit hook was installed. An in-app
browser was selected but no tab was opened and no browser test was run.
