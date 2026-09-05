# VideoToolbox MPEG-4 playback fix — review record and canary handoff

**Status:** implemented for code review; original-file canary and deployment
remain pending · **Source report:**
[GitHub issue #913](https://github.com/pjunod/plurx/issues/913) · **Forgejo
branch:** `codex/issue-913-videotoolbox-mpeg4` · **Base:** `d9fb4daf` ·
**Written:** 2026-09-05

Companion to [VALIDATION.md](VALIDATION.md) (what each kind of evidence proves)
and [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (how an ordinary change
reaches Forgejo). This document records the reviewed incident decision, the
implemented routing and cache contracts, and the canary evidence still needed
before closing #913. Read §1–§4 for the code review, §5 for current evidence,
and §6 before calling the playback incident resolved. If implementation seems
to require an AVI demux change, an MP3 repair, a global acceleration change, or
a new retry owner, stop: those are outside this incident patch.

## 1. Decision — avoid one implicated decoder pair, not a workload class

When the selected output encoder is VideoToolbox and the probe reports
`video_codec == Some("mpeg4")`, use FFmpeg's software video decoder. Keep the
independently selected VideoToolbox output encoder. Apply the rule regardless
of container, profile, resolution, or bit-depth metadata.

```text
 selected encoder is VideoToolbox?
        │
        ├── no ─────────────────────────────▶ preserve existing decode policy
        │
        ▼ yes
 probed video codec is exactly `mpeg4`?
        │
        ├── no ─────────────────────────────▶ preserve VideoToolbox decode
        │
        ▼ yes
 software video decode ──▶ filters ──▶ h264_videotoolbox encode
        │
        └── affected recipe receives
            `videotoolbox-mpeg4-software-v1`
```

This is a compatibility rule for the codec/backend pair implicated by #913.
It is not a claim that VideoToolbox universally lacks MPEG-4 Part 2 support,
and it is not a new definition of which sources are cheap. The reported SD
source should make the workaround inexpensive, but decode cost does not decide
whether the compatibility rule applies.

The reviewed proposal used `heavy_source` as a blanket gate and would have
software-decoded every non-heavy VideoToolbox input. That would also have
changed 1080p H.264 · 4K H.264 · cropped 3840×1600 SDR HEVC. Issue #913
provides no evidence for changing those paths, so the implementation does not.

## 2. Failure boundary — input decode changes while output encode stays put

The report identifies a 624×352, 25 fps AVI whose video probe is MPEG-4 Part
2, Advanced Simple Profile, with XVID fourcc. The affected FFmpeg 7.1.4-Jellyfin
session repeatedly logged video decoder failures including `No frame decoded?`
and produced macroblocked or green output. VLC and Plex rendered the stored
file correctly.

[`hls_args`](../crates/plurx-core/src/transcode/mod.rs) assembles input decode
arguments and output encoder arguments independently. Before this patch,
selecting `Encoder::VideoToolbox` caused the ordinary decode path to add:

```bash
-hwaccel videotoolbox               # before -i: input decode
-c:v h264_videotoolbox              # after -i: output encode
```

For a probed `mpeg4` input, the corrected command omits only the first pair:

```bash
-i episode.avi
-c:v h264_videotoolbox
```

The implementation uses
[`videotoolbox_mpeg4_software_decode`](../crates/plurx-core/src/transcode/encoder.rs)
as the shared pure predicate. It checks the selected encoder and the canonical
probe value. It does not infer a codec from `.avi`, treat every MP4-contained
codec as MPEG-4 Part 2, or recognize aliases not produced by the probe.
Missing, differently cased, and unrecognized codec metadata therefore retain
the pre-fix VideoToolbox behavior.

### 2.1 Existing precedence and escape hatches remain intact

[`hls_args`](../crates/plurx-core/src/transcode/mod.rs) still resolves a
renderer requirement before ordinary decode setup:

1. A renderer that requires software-decoded frames receives them.
2. A vendor graph that owns a matching hardware surface path retains its own
   decode arguments.
3. The ordinary decode setup applies the operator override and compatibility
   rule.

`PLURX_HWDECODE=off`, `0`, `false`, or `no` still forces software decode in
the ordinary path while retaining hardware encoding. The integration test
runs each value in a child process because environment variables are
process-wide; mutating them inside a parallel test process would make the
result timing-dependent.

This patch does not reconcile the older fact that vendor-owned graphs can
bypass the ordinary environment switch. That belongs to the explicit decode
planning work in §7.

## 3. Cache identity — old decoded pixels cannot masquerade as corrected output

Changing only FFmpeg arguments would leave a correctness hole. A completed
cache entry or retained producer prefix made with VideoToolbox MPEG-4 decode
could otherwise be reused after the routing correction, making a fixed binary
serve old pixels.

[`Recipe::hash`](../crates/plurx-core/src/transcode/recipe.rs) now derives an
optional named policy field from the same predicate as command generation:

| Field | Affected value | Unaffected value |
|---|---|---|
| `decode_policy` | `videotoolbox-mpeg4-software-v1` | omitted |

The field enters the existing length-delimited SHA-256 construction. Omitting
it for unaffected recipes is deliberate: inserting an empty field would still
move every key. `CACHE_FORMAT_VERSION` therefore remains at 2 and unrelated
encoder/codec combinations retain their existing identities.

The stable revision is a contract, not prose. If this pair changes decode
policy again, assign another revision; do not silently reinterpret `v1` or
make old affected artifacts eligible again.

### 3.1 One constructor covers every cache consumer

The daemon's
[`effective_recipe`](../crates/plurxd/src/transcode.rs) remains the only
manager-level recipe constructor. It sets the encoder before `Recipe::hash`
derives the policy field. The resulting hash is used unchanged by:

| Consumer | How the corrected key governs it |
|---|---|
| Live lookup | Old complete output is a miss |
| Verified cache offers | Only corrected identity can be offered |
| Speculative production | Work is claimed under the corrected key |
| Offline production | Package identity records the corrected key |
| Retained producer prefix | Staging directory is derived from the corrected key |
| Publication | Final directory and cache row use the corrected key |

The consumer regression seeds both forms of old state. It proves that an old
complete row is not served and that an old staging part is outside the new
staging root. It then publishes a valid fixture under the corrected key and
proves a subsequent corrected request hits it. Old bytes are left for normal
retention; invalidation is by identity mismatch, not deletion.

## 4. Regression matrix — pin the narrow change and the rejected broad one

The retained integration binary is
[`videotoolbox_decode_policy.rs`](../crates/plurx-core/tests/videotoolbox_decode_policy.rs).
It inspects exact adjacent option/value tokens on the correct side of `-i`.
A loose joined-string assertion could mistake an encoder label or filter
argument for input decoder selection.

| Case | Required result |
|---|---|
| Reported MPEG-4 ASP SD AVI | Software decode · `h264_videotoolbox` encode |
| Same MPEG-4 facts in MKV and MP4 | Same result; container is irrelevant |
| MPEG-4 with another profile and 3840×2160 geometry | Same compatibility rule |
| 1920×1080 H.264 | Existing VideoToolbox decode and encode |
| 3840×2160 H.264 | Existing VideoToolbox decode and encode |
| 3840×1600 SDR HEVC | Existing VideoToolbox decode and encode |
| 3840×2160 HDR HEVC | Existing VideoToolbox decode and encode |
| Renderer requiring software decode | Renderer requirement still wins |
| Four recognized operator override values | Software decode in isolated subprocesses |
| MPEG-4 with NVENC | Existing CUDA decode and NVENC encode |
| Missing or unrecognized codec metadata | Existing VideoToolbox decode |
| Affected old/new recipe | Keys differ and revision is present |
| Unaffected old/new recipe | Keys remain equal and revision is absent |

The H.264 and cropped SDR HEVC controls are deliberate mutation sentinels.
They fail under the rejected `Encoder::VideoToolbox if heavy` proposal, even
though the reported MPEG-4 case would pass under it.

The daemon consumer test is
`transcode::tests::videotoolbox_mpeg4_policy_rejects_old_cache_and_retained_prefix`.
It checks actual lookup, staging, publication, and second-request behavior
without requiring an installed FFmpeg binary.

## 5. Validation evidence — code proof is complete; playback proof is not

All recorded Rust commands use the rustup-selected repository toolchain and a
dedicated target directory. Results below describe this branch at base
`d9fb4daf`; they must be rerun if the base or patch changes.

| State | Command | Result and meaning |
|---|---|---|
| Passed | `rustc +1.97.1 --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Passed before editing | `cargo +1.97.1 check -p plurx-core --lib` | Baseline compiler loop established; 15 pre-existing dead-code warnings |
| Passed | `cargo +1.97.1 test -p plurx-core --test videotoolbox_decode_policy` | 13 passed · routing and targeted key behavior |
| Passed | `cargo +1.97.1 test -p plurxd --bin plurxd transcode::tests::videotoolbox_mpeg4_policy_rejects_old_cache_and_retained_prefix -- --exact` | 1 passed · old complete/prefix isolation and corrected hit |
| Passed | `cargo +1.97.1 test -p plurx-core transcode::recipe::tests` | 8 passed · existing recipe properties and literal golden key unchanged |
| Passed | `cargo +1.97.1 fmt --all -- --check` | Formatting gate |
| Passed | `cargo +1.97.1 check -p plurxd --all-targets` | Daemon and all targets compile |
| Passed | `cargo +1.97.1 clippy -p plurxd --all-targets -- -D warnings` | Denied-lint gate |
| Passed | `make validation-lint` | 23 points · 28 checks · 1,372 audited files |
| Base-blocked | `make check` through the ordinary commit hook | History audit stops at pre-existing commit `8de409c0`, which has no regression-evidence record; the hook does not reach this patch's Rust checks |
| Environment-blocked | `cargo +1.97.1 test -p plurxd --bin plurxd` | 1,563 passed · 149 failed · 3 ignored; failures are dominated by Homebrew FFmpeg missing `libx265.216.dylib` and sandbox-denied socket binds, with one unrelated slow-link timing failure; the new cache test passed in this run |
| Passed | `git diff --check` | Patch hygiene |
| Passed | `scripts/prove-fix` routing run | With only pre-fix `transcode/mod.rs` restored, the retained reported-file test fails because `-hwaccel videotoolbox` reappears before `-i` |
| Passed | `scripts/prove-fix` cache run | With only pre-fix `transcode/recipe.rs` restored, the retained affected-key test fails because the corrected recipe collapses back to the pre-fix identity |
| Pending | Original-file canary on affected macOS node | Only evidence that reported pixels are repaired |

Argument tests establish routing. Cache tests establish reuse isolation.
Compiler, lint, and daemon tests establish source health. None of those prove
that the original episode's picture is clean.

## 6. Canary acceptance — compare one variable on the affected node

Use the original episode and the exact FFmpeg 7.1.4-Jellyfin binary reported
by the incident. Record the binary path and the complete first version line.
Use fresh output directories and bypass cache for both runs.

Hold source · output encoder · filters · target rung · audio track · subtitles
· playback positions constant. Change only input decode acceleration:

1. Run the old command with `-hwaccel videotoolbox`, then the corrected command
   without it. Preserve both complete argument vectors and decoder logs.
2. Start at zero and seek to at least two later positions. AVI packet and
   keyframe boundaries can make a later position fail when startup does not.
3. Compare motion for macroblocks and green regions. Preserve comparable
   frames or short permitted samples when practical.
4. Record first-segment and first-frame latency · sustained speed · CPU use ·
   rebuffering over the same observation interval.
5. Verify the observed input decoder separately from the retained
   `h264_videotoolbox` output encoder. An Apple encoder label does not identify
   the decoder.
6. Confirm sustained MPEG-4 decoder failures disappear. Record MP3 warnings
   separately and check audible continuity and A/V synchronization.
7. Play one ordinary H.264 control and one known 4K HDR HEVC control. Confirm
   their existing decoder selection and startup behavior remain intact.

**Pass:** the original picture is clean at every tested position, hardware
encoding remains selected, sustained video-decode errors are absent, startup
meets the deployment's existing deadline, and playback sustains without new
rebuffering. Record the deadline and measurements; do not invent a new global
target for this patch.

**Fail:** corruption persists, the intended software decoder was not used, or
the corrected stream cannot sustain playback. Preserve commands, source probe,
logs, and measurements before widening the workaround.

The original media is not in this worktree. Until the controlled replay is
recorded, the branch is ready for code review and canary packaging, not for
incident closure.

## 7. Follow-up issue draft — model decode policy and decoder health explicitly

**Recommended title:** Model decode policy independently and recover from
sustained decoder failures

Create this as a separate Forgejo work item after the incident patch. It is
required architecture work, but it should not delay the bounded compatibility
fix or be folded into #913 without its own review.

### 7.1 Resolve one decode plan for every consumer

Introduce a resolved decode plan independent of the output encoder. It must
carry backend/software selection · stable reason · surface and metadata
requirements · policy revision · frozen retry alternative · resource estimate.
The same value must drive command generation, cache identity, logs, admission,
and retry construction.

Preserve current backend behavior initially. Reconcile renderer constraints,
the operator switch, capability evidence, and compatibility exclusions once,
rather than letting pipeline and ordinary decode paths answer separately.
Record requested and observed decoders separately when observation is possible.

**Acceptance:** tests cover every encoder family, renderer override, absent
metadata, and operator policy. No graph may silently bypass an incompatible
software-decode request or retain filters that require unavailable surfaces.

### 7.2 Turn sustained decoder failures into owned recovery events

Observe stream-specific video decoder failures even while segment progress
continues. Separate video from audio diagnostics, expand repeated-message
summaries, and scope counters to one producer attempt. Derive the threshold
from retained failures and tolerant controls.

Use the existing playback-control owner for transitions. Before publication,
it may install at most one frozen software-decode alternative while retaining
the hardware encoder, provided renderer/output grade stays valid and CPU
capacity is admitted. After publication, use coordinated session replacement;
never rewrite a published HLS timeline in place.

**Acceptance:** retained tests cover continuing progress with video errors,
isolated MP3 warnings, repeated summaries, pre/post-publication failures, stale
attempt observations, cancellation, retry exhaustion, resource release, and
failure-marked artifacts that must not enter the reusable cache.

### 7.3 Qualify broader policy with workload measurements

Compare MPEG-4 · H.264 · HEVC SDR · HDR10 · supported Dolby paths, including
4K H.264, cropped widescreen HEVC, higher frame rates, and concurrent sessions.
Record input/output geometry, frame rate, exact FFmpeg build, selected and
observed paths, startup latency, speed, CPU, and rebuffering.

**Acceptance:** every newly preferred path preserves pixels and metadata,
meets the existing startup budget, sustains tested concurrency, and has cache
identity plus resource-accounting coverage.

## 8. Delivery gates — do not collapse code readiness into incident closure

| Gate | Required evidence |
|---|---|
| Ready for code review | Narrow routing · targeted recipe revision · focused regressions · current-base compile/lint/test evidence |
| Ready for fleet rollout | Forgejo merge gate · controlled original-file canary acceptance |
| #913 resolved | Affected node runs corrected build · original and controls pass · old cache cannot mask behavior |
| Broader decoder work complete | Separate §7 acceptance criteria pass |

A full binary rollback also restores the old key construction and can make old
affected output eligible again. Prefer a forward correction. If decode policy
changes, assign a new targeted revision rather than reusing the old key.

## 9. Non-goals — preserve the reviewed boundary

- **No AVI demux or MP3 repair.** Keep audio warnings in evidence; investigate
  audible loss or drift separately.
- **No client or HLS redesign.** The reported boundary is server-side decode.
- **No blanket acceleration disablement.** It changes unrelated workloads and
  does not uniformly govern vendor-owned graphs.
- **No FFmpeg upgrade as the incident fix.** It changes another variable and
  requires independent qualification.
- **No reinterpretation of `heavy_source`.** That helper also controls filter
  selection and is not a decoder capability inventory.
- **No automatic decoder recovery in this patch.** That work needs the owner,
  publication fences, admission, and failure tests in §7.
