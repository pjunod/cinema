# VideoToolbox decode review — the fix to build and the follow-up it requires

**Status:** review decision and implementation handoff; no implementation or
deployment performed by this review · **Written:** 2026-09-05 · **Source:**
[issue #913](https://github.com/pjunod/plurx/issues/913) · **Reviewed code:**
`0377da04ccdd9c53875b603d13d16a10d7ae711a`

Companion to [VALIDATION.md](VALIDATION.md) (required evidence) and
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (compile, review, and merge
workflow). This document answers what the requester should change in the
proposal titled “AVI VideoToolbox playback fix — keep cheap legacy decode on
the CPU.” Read §1 for the decision, implement §3–§6 for the incident fix, and
use §7 as the separate architectural follow-up. Re-verify source interfaces
against the implementation base before editing; the reviewed branch and the
checkout's `main` are different snapshots.

## 1. Decision — narrow the routing change and close the cache gap

**Do not merge the proposed blanket `Encoder::VideoToolbox if heavy` guard as
the resolution of #913. Replace it with a VideoToolbox/MPEG-4 Part 2
compatibility rule, retain hardware encoding, and change the affected cache
identity. Confirm the repaired picture with the reported media before
declaring the incident resolved.**

The rule to ship is:

> When a transcode selects VideoToolbox encoding and its probed video codec
> is `mpeg4`, use FFmpeg's software decoder. Apply this across containers,
> profiles, and resolutions. Keep `h264_videotoolbox` as the output encoder.
> Preserve the existing routing of other codecs and encoder families.

This is a codec/backend compatibility decision. The source being SD explains
why the reported case should be inexpensive; cheapness is not a prerequisite
for avoiding the implicated decoder path. `mpeg4` here means MPEG-4 Part 2,
not every codec that can be stored in an MP4 file, and not H.264/AVC.

The immediate deliverable has four parts:

1. **Route MPEG-4 Part 2 around VideoToolbox decode.** The same elementary
   stream remains covered after remuxing, so an AVI filename exception is
   unnecessary.
2. **Invalidate affected output by recipe mismatch.** A cached transcode or
   retained production prefix made under the old decision must not be
   reused under the corrected decision.
3. **Retain behavioral regression evidence.** Cover routing, preservation of
   unrelated paths, and cache lookup/resume isolation.
4. **Produce playback evidence using the affected FFmpeg build.** Confirm
   clean pictures and seeks while hardware encoding remains selected.

Separately, undertake the explicit decode-planning and decoder-health work
in §7. It is required follow-up work, but the entire recovery redesign should
not delay a verified, bounded compatibility fix.

### 1.1 Why the recommendation is narrower than the original proposal

The original proposal treats an existing cost heuristic as a general safety
boundary. It is neither a decoder capability inventory nor a complete model
of decode cost. Applying it more widely changes unrelated workloads without
evidence that those changes help.

The alternative chosen here limits immediate behavior changes to the
implicated codec/backend pair and addresses the concrete integration defect
in cache reuse. The broader design gets its own tests and measurements rather
than being embedded implicitly in a one-line guard.

This deliberately supersedes the earlier conditional suggestion to accept
the broad guard after wider performance testing. For #913, the better
delivery choice is to avoid that unrelated policy change altogether.

## 2. Evidence — what is established and what remains unproved

| Finding | Evidence | Consequence |
|---|---|---|
| The report concerns legacy MPEG-4 Part 2 | Issue #913 reports `mpeg4`, Advanced Simple Profile, XVID, AVI, 624×352, 25 fps | A codec/backend rule addresses the reported input without depending on its filename |
| The VideoToolbox decode path is strongly implicated | Repeated `No frame decoded?` and MPEG-4 decoder failures; FFmpeg's VideoToolbox code emits that message when a pixel buffer is missing | Removing this decode path is a supported mitigation hypothesis |
| Hardware encode need not be removed | Input acceleration flags and output encoder flags are constructed separately | Retain the output encoder while changing input decode |
| The proposed guard changes more than legacy decoding | `heavy_source` admits only HEVC aliases with HDR or height ≥2160 | 4K H.264 and 3840×1600 SDR HEVC also lose requested VideoToolbox decode |
| Cache identity does not express this policy change | `PipelineDigest` includes the FFmpeg build and encoder; `Recipe` includes the filter pipeline, but no decode-policy revision | Identical lookup inputs can select an entry made before the correction |
| Decoder errors are logged without becoming health events in this path | `spawn_ffmpeg` drains stderr through `log_ffmpeg_stderr` | Continued output can conceal persistent decoder failures from recovery |
| The original episode was not replayed in this review | The proposal reports the source absent from its worktree | Neither the original two argument tests nor this review proves repaired pixels |

The exact failure mechanism is not fully established. FFmpeg 7.1's
[VideoToolbox frame handler](https://www.ffmpeg.org/doxygen/7.1/videotoolbox_8c_source.html)
reports the missing frame buffer and returns an error. That supports the
decode diagnosis; it does not demonstrate that every reported damaged frame
was passed onward by that specific error branch. Avoid turning a supported
diagnosis into a claim of a completed controlled reproduction.

The review also does not establish that this episode has an existing cached
transcode, that every MPEG-4 profile fails in VideoToolbox, or that the broader
guard necessarily causes a measurable slowdown. The recommendations concern
reachable behavior and missing evidence, not invented production outcomes.

### 2.1 Source map for the implementer

| Contract | Source and symbol | Reviewed behavior |
|---|---|---|
| Input acceleration | [transcode/mod.rs](../crates/plurx-core/src/transcode/mod.rs), `decode_setup` | Chooses acceleration primarily from encoder family; checks `PLURX_HWDECODE` |
| Workload heuristic | [transcode/mod.rs](../crates/plurx-core/src/transcode/mod.rs), `heavy_source` | HEVC aliases and HDR or height ≥2160 |
| Command assembly | [transcode/mod.rs](../crates/plurx-core/src/transcode/mod.rs), `hls_args` | Renderer requirements precede ordinary decode setup |
| Renderer requirements | [pipeline.rs](../crates/plurx-core/src/transcode/pipeline.rs), `requires_software_decode`, `decode_args`, `for_session` | Dolby renderers can require software; vendor graphs own matching hardware surfaces |
| Durable output identity | [recipe.rs](../crates/plurx-core/src/transcode/recipe.rs), `PipelineDigest`, `Recipe::hash` | `CACHE_FORMAT_VERSION` is 2 in the reviewed tree |
| Cache producer and consumer | [daemon transcode.rs](../crates/plurxd/src/transcode.rs), `effective_recipe`, `serve_cached`, `produce_normalized` | Common recipe constructor supplies lookup and production identity |
| Process diagnostics | [daemon transcode.rs](../crates/plurxd/src/transcode.rs), `spawn_ffmpeg` | Stderr logging and stdout progress observation are separate |
| Frozen retry | [daemon transcode.rs](../crates/plurxd/src/transcode.rs), `PrepublicationTranscodeRetry::build` | GPU-filter fallback can retain the encoder; CPU-filter hardware sessions fall back to software encode |
| Publication boundary | [playback_control.rs](../crates/plurxd/src/playback_control.rs), `PrepublicationRetryState` | Publication closes eligibility for an in-place retry |
| CPU accounting | [admission.rs](../crates/plurxd/src/admission.rs), `Workload` | Existing classifications do not constitute a general independent decoder budget |

These relative links navigate the repository. The findings above describe
the reviewed commit; use symbol names to locate them if lines have moved.

## 3. Incident fix — the exact routing contract

### 3.1 Preserve the established interfaces and renderer constraints

The relevant existing interfaces in the reviewed code are:

```rust
pub fn heavy_source(source: &MediaFile) -> bool;

fn decode_setup(
    encoder: Encoder,
    source: &MediaFile,
) -> (Vec<String>, Option<String>);

pub fn hls_args(
    source: &MediaFile,
    encoder: Encoder,
    opts: &TranscodeOptions,
    pacing: Pacing,
    out_dir: &str,
) -> Vec<String>;
```

These are signatures copied from the source, shown without bodies. Re-verify
them in [transcode/mod.rs](../crates/plurx-core/src/transcode/mod.rs) at build
time. The first `decode_setup` result is input-side arguments; the second is
an optional hardware download filter prefix. Software decode returns no
acceleration arguments and no hardware download prefix.

Implement the new compatibility condition inside the ordinary decode policy.
Do not retain the broad VideoToolbox `heavy` guard and add another exception
beside it: replace that guard. A shared, pure predicate for the affected
codec/backend pair is appropriate if command generation and recipe identity
both need it.

The routing requirements are:

| Input or constraint | Required result |
|---|---|
| VideoToolbox encoder + probed `video_codec == Some("mpeg4")` | Software video decode; retain `h264_videotoolbox` encode |
| Same MPEG-4 source in AVI, MKV, or MP4 | Same decode decision |
| Same MPEG-4 codec with a different profile, size, or bit-depth field | Same compatibility rule; do not make it depend on SD classification |
| VideoToolbox + H.264 or HEVC | Preserve pre-proposal routing, subject to existing overrides and renderer constraints |
| NVIDIA, QSV, VAAPI, software encoders | Preserve pre-proposal routing |
| Renderer requires software decode | Continue satisfying that renderer requirement |
| Renderer owns a vendor hardware surface path | Preserve valid pipeline ownership; never remove its decoder while retaining filters that require those surfaces |
| Missing or unrecognized codec metadata | Preserve existing behavior; do not infer MPEG-4 Part 2 from an extension or invent aliases |

The existing environment escape hatch accepts `PLURX_HWDECODE=off`, `0`,
`false`, or `no` in `decode_setup`. Preserve that behavior. Do not describe
the switch as universally enforced across vendor-owned graphs: `hls_args`
can use `Pipeline::decode_args` without calling `decode_setup`. Reconcile
that pre-existing inconsistency in §7 rather than changing pipeline
precedence incidentally in this fix.

**Acceptance:** a real session's generated arguments select software decode
for the reported codec while retaining the hardware encoder. The input
path's container suffix does not affect the result.

### 3.2 Use a compatibility rule without claiming a universal capability fact

Document the reason as “avoid the VideoToolbox MPEG-4 decode path implicated
by #913.” Do not claim that MPEG-4 is universally unsupported by VideoToolbox
or that all other sources are cheap. FFmpeg exposes an MPEG-4 VideoToolbox
path; the problem is this path's behavior on the reported stream.

Covering every MPEG-4 profile is a deliberate compatibility trade-off. A
more selective restriction would require reliable stream features and
reproduction evidence that this incident does not yet provide. Revisit the
rule only with that evidence and a cache-policy revision.

## 4. Cache identity — prevent reuse of pre-fix output

### 4.1 Add a targeted policy revision to affected recipes

Add a named decode-policy revision field to recipes for VideoToolbox/MPEG-4
transcodes. For example, the stable serialized value could be
`videotoolbox-mpeg4-software-v1`. This value is a proposed new contract, not
an existing field or configuration setting.

Use a named field in the existing length-delimited hash construction. Share
the codec/backend predicate with the routing decision so the affected set
cannot drift between the command and the key. A conservative cache miss for
an affected session that already used the operator override is acceptable.

Prefer this targeted addition over a global cache-format bump: unrelated
encoders and codecs do not need to regenerate their output for this fix.
Do not hash an output directory, temporary path, session ID, or the complete
FFmpeg command; those contain execution details that would defeat reuse.

The new identity must propagate through the common recipe constructor to
live cache lookup, speculative production, offline production, retained
part lookup/resume, and publication. Inspect these consumers in the current
base; adding a field to a type without feeding the hash is not sufficient.

**Acceptance:** holding file identity, options, FFmpeg build, and encoder
constant, an affected recipe has a different key from the old policy.
Unaffected recipes retain their previous keys.

### 4.2 Prove the consumer behavior, not only two different hashes

Seed an old-policy cache entry and request the same affected transcode under
the new policy. The request must miss that entry and prepare corrected
production. Seed an old retained production prefix and verify it cannot be
resumed into the new artifact. Assert that new output is published under the
new key and that a subsequent corrected request can hit it.

Old entries can remain on disk and age out through normal retention. Checksums
on those entries establish byte integrity, not visual correctness. Deleting
the reported file's cache manually is useful for diagnosis but does not
replace invalidation in the product.

During a mixed-version rollout, a corrected node must not read an affected
old-policy entry even if an older node still publishes it. Older nodes can
still exhibit the original defect until upgraded; this patch cannot change
their behavior remotely.

## 5. Evidence — the tests and playback comparison required

### 5.1 Retain a routing and cache regression matrix

Expand the proposed integration test binary
`crates/plurx-core/tests/videotoolbox_decode_policy.rs`. It exists on the
reviewed implementation branch; re-create or adapt it on the intended base.
Test names should describe MPEG-4 compatibility rather than imply a global
light-source policy.

| Case | Required assertion |
|---|---|
| Reported MPEG-4 ASP SD AVI | No video hardware decode request; output uses `-c:v h264_videotoolbox` |
| Same MPEG-4 facts with MKV and MP4 containers | Same software decode result |
| MPEG-4 with changed profile and larger geometry | Compatibility rule still applies |
| 1080p H.264 | VideoToolbox decode and encode remain as before the broad proposal |
| 3840×2160 H.264 | No unintended loss of VideoToolbox decode |
| 3840×1600 SDR HEVC | No unintended loss of VideoToolbox decode |
| 3840×2160 HDR HEVC | Existing hardware decode and encode remain |
| Renderer requiring software decode | Its metadata-preserving decode requirement still wins |
| Operator decode override | Existing recognized values retain their documented behavior for this path |
| Unaffected encoder family | No new codec exclusion is applied to that family |
| Affected old/new recipe | Keys differ; old complete entry and retained prefix are not reused |
| Unaffected recipe | Existing golden identity remains unchanged |

Assert argument tokens and option placement around the input, rather than
only a substring anywhere in the command. Test the production builder and
its relevant session pipeline selection so a helper-only assertion cannot
miss a higher-priority path.

Isolate environment-dependent tests in subprocesses or use the repository's
existing isolation mechanism. Changing a process-wide environment variable
while unrelated tests run in parallel makes evidence unreliable.

Use [scripts/prove-fix](../scripts/prove-fix) to retain discrimination against
the pre-fix production implementation. The MPEG-4 routing test should reject
the unconditional VideoToolbox decoder; cache evidence should reject the
old identity. Restoring only the old decode file does not prove cache
invalidation, so record those as distinct claims.

### 5.2 Compare the same playback with only decode acceleration changed

Use the affected macOS node, the reported source, and the same FFmpeg binary
for both runs. The incident reports `7.1.4-Jellyfin`; tests against an
unrelated Homebrew build are useful additional evidence but do not reproduce
that deployment. Record the exact binary path and version/build string.

Hold the source, output encoder, filters, target rung, audio track, subtitle
selection, and playback position constant. Compare the original command
with the command using software video decode. Use fresh output directories
and ensure a cache hit cannot bypass either command.

For the corrected run:

1. Start at the beginning, then seek to at least two later positions.
2. Inspect motion for macroblocks and green regions. Preserve comparable
   before/after frames or short permitted samples when possible.
3. Record first-segment and first-frame latency, sustained transcode speed,
   CPU use, and any rebuffering during the same observation interval.
4. Verify the actual input decode selection and retained
   `h264_videotoolbox` output selection. An “Apple VideoToolbox” encoder label
   alone does not identify the decoder.
5. Confirm repeated video-decode errors disappear. Record MP3 warnings
   separately and check audible continuity and A/V synchronization.
6. Play a known 4K HDR HEVC control and an ordinary H.264 transcode; confirm
   their existing decoder selection and startup behavior remain intact.

**Pass:** the corrected source renders cleanly at all tested positions,
retains hardware encoding, has no sustained video-decode failure sequence,
starts within the deployment's existing startup deadline, and sustains
playback without new rebuffering. Record the applicable deadline and measured
values in the evidence; do not invent a fleet-wide latency target here.

**Fail:** visual corruption persists, the command does not use the intended
decoder, or corrected playback cannot sustain the requested stream. Preserve
the command, source probe, and logs, and investigate before widening the
workaround to additional codecs or disabling encoding acceleration.

If the original media is unavailable, synthetic MPEG-4 clips can exercise
the pipeline, but label original-file acceptance as pending. The requester
can perform the controlled canary replay without distributing the source.

## 6. Delivery — land the incident fix with explicit completion gates

### 6.1 Establish the compiler loop before Rust changes

Follow [AGENTS.md](../AGENTS.md) and
[AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md). Verify the repository-pinned
Rust 1.97.1 compiler before editing Rust. If this host cannot run it, use the
documented source-only archive loop; transfer no repository credentials.

The focused command for the retained routing test binary is:

```bash
rustc +1.97.1 --version  # Verify the actual compiler selected.
cargo +1.97.1 test -p plurx-core --test videotoolbox_decode_policy
```

Run the focused recipe/cache tests added by the implementation and record
their exact commands and counts. Then run the applicable repository checks,
including formatting, daemon compilation, denied lints, and affected tests:

```bash
cargo +1.97.1 fmt --all -- --check
cargo +1.97.1 check -p plurxd --all-targets
cargo +1.97.1 clippy -p plurxd --all-targets -- -D warnings
cargo +1.97.1 test -p plurxd --bin plurxd
git diff --check
```

**How to read the evidence:** passing argument tests establish routing;
passing cache tests establish reuse isolation; passing Rust checks establish
compile/test health. Only §5.2 establishes the reported picture is repaired.
The previously reported Homebrew FFmpeg linkage failures and denied socket
binds are missing suite evidence, not passes. Resolve the environment or run
those checks on a suitable host before claiming that evidence.

Re-run against the exact intended base after it moves. Follow the current
ordinary-change gate for the incident fix. If §7 becomes a multi-task effort,
use the effort branch and promotion workflow documented in the repository.

### 6.2 Separate code readiness, canary acceptance, and incident closure

| Gate | Required evidence |
|---|---|
| Ready for code review | Codec-specific routing, targeted recipe revision, focused regressions, current-base compile/lint/test evidence |
| Ready for fleet rollout | Repository merge gate satisfied and controlled original-file canary acceptance recorded |
| Incident #913 resolved | Affected node runs the corrected build; original episode and control playback pass; old cache cannot mask the routing change |
| Broader decoder work complete | The independent acceptance criteria in §7 pass |

A build can be prepared for controlled canary validation while original-file
acceptance is pending. Do not use “implemented,” a passing CI run, or a first
frame alone as evidence that the incident is resolved.

Update the original proposal to match this decision. Remove the blanket
“all light VideoToolbox sources use CPU decode” claim, the assertion that
there is no cache identity impact, and the assumption that exactly two
argument tests constitute the complete regression proof. Distinguish recorded
test results from commands an implementer still needs to run.

### 6.3 Rollback must not make old corrupted output eligible again

Prefer a forward correction if the new policy exposes a problem. Keep old
affected artifacts ineligible. If decode policy changes again, issue another
policy revision rather than reusing the old key or silently changing the
meaning of the new one.

A full binary rollback also rolls back key construction and can resurrect
old affected entries; treat that as an explicit operational consequence.
Global `PLURX_HWDECODE=off` is not the normal rollback for this fix because it
changes other workloads and does not uniformly govern vendor-owned graphs.

## 7. Required follow-up — make decode choice and decoder health explicit

The standalone proposal and implementation milestones now live in
[DECODER_SELECTION_AND_RECOVERY_PLAN.md](DECODER_SELECTION_AND_RECOVERY_PLAN.md).
Use that document to execute the broader effort; this section records the
scope established by the incident review.

**Recommended follow-up title:** “Model decode policy independently and
recover from sustained decoder failures.” This review supplies the scope; it
has not created an issue, assigned an owner, or sent a message externally.

### 7.1 Resolve one effective decode plan before executing the session

Introduce an explicit resolved decode decision alongside the selected
encoder and renderer. The exact Rust type is an implementation choice, but
its contract must include:

| Field or property | Purpose |
|---|---|
| Software or specific hardware backend | Identify the requested decoder path independently of the output encoder |
| Stable reason | Explain compatibility exclusion, renderer requirement, operator override, or measured benefit |
| Surface and metadata requirements | Preserve filter compatibility, bit depth, and required Dolby metadata |
| Policy revision | Prevent corrected output from colliding with an older decision's artifacts |
| Frozen retry alternative | Supply a valid next attempt without rereading mutable policy after failure |
| Resource estimate | Account for CPU decode work even when encoding uses hardware |

Resolve renderer constraints, compatibility exclusions, operator policy,
backend capability evidence, and measured cost together. Capability means
the input codec/profile/pixel format can use that backend on this node and
FFmpeg build; detecting a working output encoder is insufficient evidence.

Make this resolved plan the common input to command generation, cache
identity, logging, admission, and retry construction. Keep human-readable
reason text out of cache identity; editorial changes should not invalidate
output. When available, record the observed decoder separately from the
requested one, because requested acceleration does not prove its use.

Preserve existing backend behavior initially, including NVIDIA's current
exception, and make exceptions explicit. Do not impose one HEVC-only cost
threshold across every backend. Evaluate source dimensions, frame rate,
codec/profile, bit depth, renderer requirements, and node measurements before
changing performance policy. Define conservative handling of absent facts.

**Acceptance:** the same resolved plan generates the command, stable cache
identity, resource classification, and diagnostic selection fields. Tests
cover all encoder families, renderer overrides, absent metadata, and the
operator switch. The switch either selects a compatible software-decode
pipeline or yields an explicit incompatibility; it cannot be silently
bypassed or leave a graph requiring unavailable hardware surfaces.

### 7.2 Feed sustained video-decode failures into the existing recovery owner

Extend observation to identify stream-specific video decoder failures even
while progress advances. If parsing stderr, handle repeated-message
compression, isolate video from audio diagnostics, and keep counters scoped
to the producer attempt. Establish the trigger threshold from retained
failure logs and tolerant-control fixtures; document the resulting threshold
and detection latency before enabling automated recovery.

Use the existing playback control owner for transitions. Do not add a
second process-spawning retry loop in the stderr reader. Attempt identity,
cancellation, stale observations, resource ownership, and publication fences
must remain authoritative.

For an eligible hardware-decode failure, allow at most one automatic retry
with software decode and the existing hardware encoder, provided that the
renderer and output contract remain valid and CPU capacity is admitted. A
repeated failure of that alternative terminates automatic retry and surfaces
an actionable failure. A renderer with no grade-preserving alternative must
fail rather than quietly change color or dynamic-range behavior.

Before publication, the existing controlled replacement mechanism may install
the frozen retry. After publication, use the coordinated session replacement
contract at the viewer's position. Never clear and rewrite a published HLS
timeline in place. Prevent failure-marked artifacts from being promoted as
successful reusable output, while retaining any already-published material
required by the existing delivery contract.

FFmpeg's [`-max_error_rate`](https://www.ffmpeg.org/ffmpeg.html) does not
terminate processing when its threshold is crossed; it changes the eventual
exit status. Conversely, global `-xerror` exits on errors and would also act
on unrelated malformed audio. Neither is a substitute for the scoped health
and recovery policy above.

**Acceptance:** retained tests simulate video errors with continuing segment
progress, isolated MP3 warnings, repeated stderr summaries, prepublication
failure, failure after publication, stale errors from a prior attempt,
cancellation during retry, and retry exhaustion. They prove at most one
automatic alternative, preserved output grade, correct resource release, and
no mixed old/new published timeline. An affected-artifact test proves that
reported decoder failure cannot be promoted as clean reusable output.

Silent visual corruption without decoder diagnostics remains outside this
health trigger's detection power. Keep real playback fixtures and visual
qualification; do not advertise log monitoring as a pixel-correctness proof.

### 7.3 Qualify policy changes against workloads and concurrency

Before broadening software decode beyond the incident rule, compare old and
proposed plans on representative MPEG-4, H.264, HEVC SDR, HDR10, and supported
Dolby paths. Include 4K H.264, cropped widescreen SDR HEVC, higher frame rates,
and concurrent sessions within configured admission limits. Add other codecs
only where the node's supported decoder inventory makes the comparison real.

Record source and output geometry, frame rate, exact FFmpeg build, selected
and observed paths, first-segment latency, sustained speed, CPU use, and
rebuffering. Separate decode cost from output encoding cost where measuring
the decision requires it.

**Acceptance:** each newly preferred path preserves required pixels and
metadata, meets the existing startup budget, and sustains the tested
concurrency. Routing changes have cache identity and resource-accounting
coverage. A comment saying “light” or a capability listing alone is not
qualification evidence.

## 8. Scope boundaries — work this handoff does not authorize implicitly

- **No AVI demux or MP3 repair change.** Keep audio warnings in the evidence;
  investigate audible loss or drift separately if it remains after decode
  correction.
- **No client compatibility or HLS format redesign in the incident fix.**
  The report supports a server decode intervention. Preserve output contracts
  while confirming that diagnosis.
- **No blanket acceleration disablement or FFmpeg upgrade as the fix.**
  Either changes additional variables and requires independent qualification.
- **No global reinterpretation of `heavy_source` in #913.** Changing that
  helper also changes filter selection; evaluate it through §7.
- **No claim that the architecture work has already been implemented.**
  This artifact is a review decision and a handoff. It contains no new Rust,
  reproduced playback results, completed deployment, or independently rerun
  compile/test results.

The request back to the implementer is therefore concrete: replace the broad
guard with the codec/backend compatibility rule, add targeted cache identity
and its consumer tests, obtain original-file acceptance, and carry the
explicit decode-plan/recovery work forward as the separate scoped follow-up.
