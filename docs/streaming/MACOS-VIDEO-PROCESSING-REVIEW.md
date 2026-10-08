# macOS video processing — independent reviews and dispositions

**Status:** continuation implementation review active · **Updated:** 2026-10-08.

Companion to the [design](MACOS-VIDEO-PROCESSING-DESIGN.md),
[implementation plan](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md) and
[execution status](MACOS-VIDEO-PROCESSING-STATUS.md). Sections 1–6 retain the
original 2026-10-07 proposal review and its author dispositions. Section 7
records the final continuation implementation review. Neither is an Opus
review or a hardware qualification receipt.

The original proposal review inspected `890bca0fc186d00f7b10a379cd7dd2f1a37dc37c`.
Its verdict was request changes; author corrections were not independently
re-approved at that stage. Implementation reviews identify their own source
candidates and must not be inferred from that documentation review.

## 1. Reviewed inputs and verdict

The reviewer inspected the full documents, local planner/recipe/recovery/
cluster seams, and primary Jellyfin HEVC-decoder and Metal-filter sources.
Original draft SHA-256 values identify the reviewed text before correction:

| Document | Original SHA-256 |
|---|---|
| Design | `d02b78bdc0b5711bcb1bfd5326639b13881bdeebefb2537e2ec6258556186e03` |
| Implementation | `554ce12acde4cd734b97642e794bac7624f9ea9eb8e326f1ada04a885d43354e` |

**Verdict:** request changes to the execution contract. The experiment is
justified; no performance result is required to approve M0/M1. Three gaps
need correction before implementation, with the Dolby enforcement requirement
specifically attached to E1. All three findings were rated P2; no P0/P1
finding was reported.

The line references below identify the original draft, not line numbers in
the revised documents. Section references point to the corrected contract.

## 2. R1 — P5 metadata must be enforced in the executing graph

**Priority:** P2 · **Original references:** design lines 207 and 416;
implementation line 381 · **Disposition:** accepted and corrected in design
§5.1 and implementation E1 (§9.1).

**Finding:** planning flags and a startup sample prove neither metadata
continuity nor strict behavior inside the running filter. A P5 source may
pass initially and lose effective Dolby metadata after a seek, frame
transition or decode/transfer defect. The pinned Metal filter's `apply_dovi`
means use metadata when present. Without it, PQ/HLG can proceed through
ordinary mapping with exit zero. That can produce incorrect pixels without
triggering the proposed failure recovery.

**Evidence:** the [Jellyfin v8.1.3-1 Metal filter patch](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.3-1/debian/patches/0046-add-vf-tonemap-videotoolbox-filter.patch)
conditionally reads `AV_FRAME_DATA_DOVI_METADATA` and otherwise accepts
SMPTE2084/HLG. The required-side-data field in
[decode.rs](../../crates/plurx-core/src/transcode/decode.rs) describes a plan;
it does not validate every frame inside FFmpeg.

**Author correction:** E1 now requires a proved strict mode or a minimal
reviewed patch enforcing required effective Dolby metadata at the renderer
boundary. Define effective metadata to include legitimate decoded reuse,
with correct frame association, rather than requiring a new RPU in every
packet. Missing/invalid required state must fail before the affected frame
reaches the encoder; existing recovery owns any replacement. Bind strict
mode and patch revision into the recipe.

Tests now explicitly cover initial absence, midstream loss, valid reuse,
stale seek state, reordered frames and ordinary HDR10/HLG controls. If this
execution contract cannot be supplied, retain the existing P5 route. This
is an executable compatibility requirement, not an offline-review gate on
the user's saved setting. SDR/HDR10 experiments can proceed independently.

**Still to prove during implementation:** the strict boundary exists in the
selected executable; rejection occurs before affected output publication;
valid reuse is not rejected; recovery does not splice differently processed
bytes into the prior artifact.

## 3. R2 — pipeline names are diagnostic, not remote execution commands

**Priority:** P2 · **Original references:** design line 482; implementation
line 294 · **Disposition:** accepted and corrected in design §5.4 and
implementation M3 (§6).

**Finding:** the draft claimed old workers decline unknown graph names through
existing compatibility handling. Current snapshot/offer validation accepts
bounded strings, and eligibility uses broad capability facts. Adding a new
enum to a new binary cannot change an old peer's interpretation.

**Evidence:** [media_pool.rs](../../crates/plurxd/src/media_pool.rs),
`snapshot_is_bounded`, `offer_is_bounded` and `snapshot_can_answer`; and
[media_sessions.rs](../../crates/plurxd/src/media_sessions.rs),
`RemoteStartRequest`, which carries presentation/session requirements rather
than a required processing graph. This establishes a false proposed
mechanism, not an observed unsafe graph transplant in today's runtime.

**Author correction:** choose the existing worker-local resolution model.
Pipeline strings on the wire remain diagnostic; detailed Mac graph classes
stay local. A worker must resolve and accept the actual requested
presentation/decoder constraints using its own current capabilities and
saved preference. A coarse advertisement is a prefilter, never execution
proof. An old worker may correctly use its incumbent graph for a presentation
that a new worker delivers with Metal.

M3 now explicitly tests old-to-new and new-to-old dispatch, unsupported
presentation refusal, capability-generation changes between offer and start,
and takeover onto a different recipe. Actual worker plan/recipe identity
controls output and cache lookup. Replanning cannot append new bytes to the
old artifact. If a required remote graph constraint becomes necessary, it
needs an explicit versioned envelope/protocol amendment, not an opaque string
under the existing version.

**Still to prove during implementation:** offer/start revalidation reaches
the actual acceptance seams; stale capability/recipe data cannot grant a
cache hit or authorize an incompatible presentation; takeover maintains its
existing publication and lease fences.

## 4. R3 — a clean install needs provisioned smoke fixtures

**Priority:** P2 · **Original references:** design line 377; implementation
lines 267 and 457 · **Disposition:** accepted and corrected in design §4.5
and implementation M1/M3/M6 (§4/§6/§10).

**Finding:** the draft required compressed fixtures but assigned neither
shipping nor generation. The offline benchmark harness could leave a developer
Mac prepared while a fresh install stays permanently unavailable. Restart
and reprobe do not fix an absent input. Unbounded first-start generation would
also violate the intended responsiveness contract.

**Evidence:** the current [pipeprobe](../../crates/plurxd/src/pipeprobe.rs)
generates and caches its own HDR10 fixture. It is not a provisioner for the
proposed collection of Mac graph classes.

**Author correction:** M1 owns a small, redistribution-safe synthetic smoke
corpus plus generator/provenance and a versioned hash/expectation manifest.
M3 embeds and loads it; M6 verifies the release on a clean offline install.
Initial aggregate compressed size budget is 2 MiB, revised explicitly if
adequate fixtures do not fit. Preparation writes verified embedded bytes
asynchronously with a separate 5 s deadline and atomic cache publication;
there is no runtime network fetch or fixture encoding.

Corrupt cached bytes are recoverable from the embedded copy. Disk/permission
failures are reported separately and can be retried after repair. Invalid
embedded data is a packaging failure. Private quality media stays outside
the runtime dependency. Later graph classes must ship their own lawful
fixtures before their runtime capability is advertised.

**Still to prove during implementation:** clean offline start, no cache,
corrupt cache, invalid manifest, failed writes, cancellation and successful
reprobe; a developer benchmark directory must not be required on the host.

## 5. Premises that survived the adversarial pass

- Shared memory removes the discrete PCIe-copy premise; conversion, copying
  and synchronization can still cost time. Measuring rather than assuming
  their importance is appropriate.
- Hardware HEVC reconstruction can coexist with CPU Dolby metadata parsing.
  The pinned decoder attaches Dolby side data and the Metal renderer consumes
  it; this motivates an experiment rather than proving a supported tuple.
- The four-way decoder/renderer comparison isolates useful changes, including
  hardware decode feeding the current CPU renderer.
- Saved preference, runtime compatibility and offline qualification are
  substantially separated; none is a substitute for the others.
- Effective recipe identity, implementation attestation, immutable running
  plans, grade-preserving fallback and segment-boundary tone-map checks cover
  important correctness risks.
- Reusing the existing recovery owner is credible. The existing
  [prepublication retry](../../crates/plurxd/src/transcode/rolling/prepublication.rs)
  already supports a renderer fallback while retaining the hardware encoder.

## 6. Verification and limits

The author ran the four existing documentation-index tests and whitespace
checks and verified local links in all three documents. The new review record
is indexed alongside the design and implementation plan. No Rust code,
FFmpeg package, daemon setting or media file changed.

The reviewer did not benchmark hardware, reproduce the dated local FFmpeg
inventory, inspect every distribution patch or establish GPU/driver behavior.
The author corrections are documentation changes, not proof that their future
implementation works. The original verdict remains request changes on the
original draft; this record reports the fixes without claiming independent
approval of the revised text.

## 7. Continuation implementation review — PR #904

**Candidate:** `2979112df3d9ae9e45e8a4d174977f56aaeb10cd` ·
**Base:** `c746b6c9b` · **State:** all findings repaired and independently
accepted, including terminal registration `54c3a3fe8`; no unit execution.

The combined candidate includes all three continuation builders. Independent
read-only reviewers cover parser/package security, immutable playback and
Dolby contracts, then Live/client/output integration. Confirmed defects return
to the owning Sol builder; corrected source is independently re-reviewed.
After acceptance, the designated merge coordinator owns the fast lane and
main merge. Missing physical-device or calibrated-display evidence remains
visible in the status page and is not treated as a source-code defect.

### 7.1 Parser and package security

Reviewer `macos_final_review_security` requests two localized P2 corrections:

| ID | Concrete failure | Architectural repair | Disposition |
|---|---|---|---|
| A1 | Package assembly validates the parser receipt but accepts replaced regular FFmpeg/FFprobe binaries alongside stale native source, dependency and capability provenance. | Validate pinned native source identity and both executable hashes in the existing pre-copy and staged-copy validator. | Repaired in `9acf814a8`; independently accepted by the security reviewer. |
| A2 | Failed WASM parser initialization occurs after setting the snapshot immutable flag but before its cleanup owner exists; temporary-file removal fails and leaks an immutable snapshot. | Establish the existing snapshot cleanup owner before the fallible parser construction, retaining ownership on every error path. | Repaired in `9acf814a8`; independently accepted by the security reviewer. |

The reviewer also inspected held-descriptor confinement, cancellation and
admission ownership, JIT/cache identity, Wasmtime import/resource bounds,
live signing inspection, sibling resolution, dSYM identity/copy checks,
native build/patch provenance, dependency source offers and regression
definitions. No additional concrete blocker was found in that scope.
Cargo-deny policy validation remains distinct from the retained dependency
license metadata audit. No review agent ran tests or compilation.

### 7.2 Immutable playback and Dolby contracts

Reviewer `macos_final_review_contracts` completed the core review with two
P2 corrections:

| ID | Concrete failure | Architectural repair | Disposition |
|---|---|---|---|
| B1 | The runtime P5 observer checks ordinary FFprobe metadata and gray output; a selected graph's stale or shuffled metadata or incorrect colored output could pass those independent checks. | Observe the actual selected graph's pre-renderer PTS-to-Dolby-metadata association and every frame's colored patches, with stale/shuffled/missing metadata and color-corruption negative controls. | Repaired in `a0a3734b`; independently accepted by the contracts reviewer. |
| B2 | The strict decoder requires current RPU metadata, but its upstream extraction only recognizes an RPU in the literal final NAL position. Valid trailing EOS/EOB can hide a present RPU from that guard. | Reproduce with a bounded legal trailing-NAL fixture, then locate the last non-EOS/EOB NAL within the access unit while retaining strict absence/malformed refusal. | Reproduced with exact strict flags: baseline 24 frames/exit 0; EOS, EOB and both yield 21 frames/exit 183. Native repair `184c937d` and runtime registration `54c3a3fe8` independently accepted. |

B1 is a qualification-authority defect, not evidence that the current GPU
misrenders. Tiny per-frame peak changes identify metadata transport but do
not establish observable per-frame pixel influence. The separate retained
apply/no-apply experiment keeps that narrower scope.

### 7.3 Live, clients and output negotiation

Reviewer `macos_final_review_delivery` identified C1 (P2): Android publishes
an HLS HEVC sample-entry claim but clears measured decoder profiles when
Display-aware Auto is off. The server correctly requires those profiles,
so ordinary HEVC Auto preference becomes dependent on an unrelated switch.
The client must retain its measured HEVC profile/geometry/rate entries when
emitting that transport claim, with a capability-document regression for
the switch-off case. Server validation and unknown-capability refusal remain
unchanged. The client-owner repair `0d6a466cd` is independently accepted.
With Display-aware Auto off, actual measured HEVC profiles, geometry and
cadence are retained only for a positive HLS transport claim. Unknown and
unclaimed HEVC remain unknown or retain prior behavior, and other codecs
are unchanged. App/test APK compilation and explicit JVM regression-source
compilation pass; no tests ran.

Scope A is independently approved after re-review of `9acf814a8`. Both
pre-copy/staged native identity checks and cleanup ownership before every
fallible immutable-snapshot initialization were verified by inspection.
Associated regression definitions cover the reported failures. This approval
is subject to the coordinator's final validation, not a claim that units ran.

B1 is independently accepted at `a0a3734b`. The re-review confirms actual
pre-mapper frame metadata, exact grouping/PTS and fail-closed missing/duplicate
records, plus colored bounds that reject neutral, swapped and raw IPT
interpretations. Existing child/output/time/cancellation bounds remain intact;
there is no added subprocess or authority owner. The broad gain allowance is
explicitly scoped to interpretation rather than precise tone-curve approval.

B2's native correction is independently accepted at `184c937d`. The reviewer
confirms the reverse scan skips only legal trailing EOS/EOB and retains RPU
position, size/layer/temporal checks and strict metadata refusal. The original
synthetic generator changes the final sample payload/size while preserving
configuration and timestamps. Required-hardware and software receipts support
the correction. Runtime registration of the three terminal positives is a
separate narrow follow-on, independently accepted at `54c3a3fe8`. Twelve
sources across five graph controls produce sixty checks, including all
original strict-loss negatives. Fixture hashes match the independently
accepted native controls. No acceptance shortcut, subprocess owner, larger
corpus cap or longer aggregate deadline was introduced.
